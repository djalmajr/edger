//! Chave de assinatura isolada por app para o provisionador sqld (EDG-54).
//!
//! Cada app recebe uma chave Ed25519 ALEATÓRIA (ring, PKCS#8 v2 gerado pela
//! própria biblioteca — sem DER/PEM/JOSE manual e sem KDF/HMAC próprio). O
//! PKCS#8 é persistido ENCRIPTADO (ChaCha20-Poly1305, nonce aleatório de 12
//! bytes) sob uma master DEDICADA, fornecida pelo chamador (`&[u8; 32]`), que
//! NÃO é a master do cofre de segredos de apps. Assim a posse remota da chave
//! pública (`DecodingKey::from_ed_components`) pode ser provada depois de um
//! restart ou de um `CREATE` com resposta 400; a linha de intenção do
//! registro não é prova.
//!
//! O módulo só usa CSPRNG e criptografia: nenhuma I/O de arquivo, env,
//! configuração ou rede. O dono (`worker`, namespace RBAC opcional e geração)
//! é parâmetro passado pelo host, não autoridade deste módulo.
//!
//! AAD (prefixo de versão do serviço + campos COM COMPRIMENTO):
//! `"edger.sqld-signer.v1\0" || u32be(len(worker)) || worker || tag ||
//! [u32be(len(ns)) || ns] || u64be(geração)`, com `tag = 0` para `None` e
//! `tag = 1` para `Some`. Isso liga app + namespace + geração; versão e
//! process id ficam de fora porque o banco é por app. Concatenações ambíguas
//! (`("ab", Some("c"))` vs `("a", Some("bc"))`, `None` vs `Some("")`) geram
//! AADs distintos.
//!
//! Limites do que é prometido: o PKCS#8 e o JWT vivem em `Zeroizing` e as
//! cópias controladas aqui são zeradas no drop; cópias internas de ring,
//! jsonwebtoken e chacha20poly1305 não têm apagamento garantido. Nenhum tipo
//! daqui implementa `Serialize`/`Display` para material secreto e os erros
//! são enums com texto estático (sem eco de entrada).

use std::fmt;

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::json;
use zeroize::Zeroizing;

/// Tamanho da master dedicada (chave ChaCha20).
pub const MASTER_KEY_LEN: usize = 32;
/// Tamanho do nonce ChaCha20-Poly1305.
pub const NONCE_LEN: usize = 12;
/// Tamanho da tag Poly1305.
const TAG_LEN: usize = 16;
/// Teto do ciphertext aceito (PKCS#8 v2 do ring tem 85 bytes + tag).
pub const MAX_CIPHERTEXT_LEN: usize = 256;
/// Teto de bytes de um worker/namespace RBAC/namespace SQL.
pub const MAX_NAME_LEN: usize = 256;
/// TTL máximo do token escopado (24 h).
pub const MAX_TTL_SECONDS: u64 = 86_400;
/// Tamanho da chave pública Ed25519.
pub const PUBLIC_KEY_LEN: usize = 32;

const AAD_PREFIX: &[u8] = b"edger.sqld-signer.v1\0";

/// Erro estático: nunca carrega chave, token, nome ou texto de entrada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqldSignerError {
    InvalidWorker,
    InvalidRbacNamespace,
    InvalidGeneration,
    InvalidNamespace,
    InvalidTime,
    InvalidTtl,
    InvalidEnvelope,
    RandomFailed,
    SealFailed,
    OpenFailed,
    KeyRejected,
    SignFailed,
}

impl fmt::Display for SqldSignerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidWorker => "sqld signer: invalid worker name",
            Self::InvalidRbacNamespace => "sqld signer: invalid rbac namespace",
            Self::InvalidGeneration => "sqld signer: generation must be non-zero",
            Self::InvalidNamespace => "sqld signer: invalid sql namespace",
            Self::InvalidTime => "sqld signer: invalid or overflowing time",
            Self::InvalidTtl => "sqld signer: ttl out of range",
            Self::InvalidEnvelope => "sqld signer: malformed sealed key envelope",
            Self::RandomFailed => "sqld signer: random source failed",
            Self::SealFailed => "sqld signer: sealing the key failed",
            Self::OpenFailed => "sqld signer: sealed key does not open",
            Self::KeyRejected => "sqld signer: key material rejected",
            Self::SignFailed => "sqld signer: signing failed",
        })
    }
}

impl std::error::Error for SqldSignerError {}

/// Identidade que o envelope liga (AAD). `worker` e `rbac_namespace` são
/// validados; `generation` deve ser diferente de zero.
#[derive(Clone, Copy)]
pub struct SqldSignerOwner<'a> {
    pub worker: &'a str,
    pub rbac_namespace: Option<&'a str>,
    pub generation: u64,
}

impl fmt::Debug for SqldSignerOwner<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqldSignerOwner")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_NAME_LEN && !value.chars().any(char::is_control)
}

fn push_field(out: &mut Vec<u8>, value: &str) {
    // `valid_text` limita a MAX_NAME_LEN, então o cast nunca trunca.
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn build_aad(owner: &SqldSignerOwner<'_>) -> Result<Vec<u8>, SqldSignerError> {
    if !valid_text(owner.worker) {
        return Err(SqldSignerError::InvalidWorker);
    }
    if let Some(ns) = owner.rbac_namespace {
        if !valid_text(ns) {
            return Err(SqldSignerError::InvalidRbacNamespace);
        }
    }
    if owner.generation == 0 {
        return Err(SqldSignerError::InvalidGeneration);
    }
    let mut aad = Vec::with_capacity(AAD_PREFIX.len() + owner.worker.len() + 32);
    aad.extend_from_slice(AAD_PREFIX);
    push_field(&mut aad, owner.worker);
    match owner.rbac_namespace {
        None => aad.push(0),
        Some(ns) => {
            aad.push(1);
            push_field(&mut aad, ns);
        }
    }
    aad.extend_from_slice(&owner.generation.to_be_bytes());
    Ok(aad)
}

/// Envelope persistível: SOMENTE nonce + ciphertext (sem plaintext).
#[derive(Clone, PartialEq, Eq)]
pub struct SealedSqldSigningKey {
    nonce: [u8; NONCE_LEN],
    ciphertext: Vec<u8>,
}

impl fmt::Debug for SealedSqldSigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealedSqldSigningKey")
            .field("ciphertext_len", &self.ciphertext.len())
            .finish_non_exhaustive()
    }
}

impl SealedSqldSigningKey {
    /// Reconstrói de bytes lidos do armazenamento; rejeita nonce de tamanho
    /// errado e ciphertext vazio, menor que a tag ou acima do teto.
    pub fn from_parts(nonce: &[u8], ciphertext: &[u8]) -> Result<Self, SqldSignerError> {
        let nonce: [u8; NONCE_LEN] = nonce
            .try_into()
            .map_err(|_| SqldSignerError::InvalidEnvelope)?;
        if ciphertext.len() <= TAG_LEN || ciphertext.len() > MAX_CIPHERTEXT_LEN {
            return Err(SqldSignerError::InvalidEnvelope);
        }
        Ok(Self {
            nonce,
            ciphertext: ciphertext.to_vec(),
        })
    }

    pub fn nonce(&self) -> &[u8; NONCE_LEN] {
        &self.nonce
    }

    pub fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }
}

/// Chave Ed25519 do app. Sem `Serialize`/`Display`; `Debug` redigido.
pub struct SqldSigningKey {
    pkcs8: Zeroizing<Vec<u8>>,
    public_key: [u8; PUBLIC_KEY_LEN],
}

impl fmt::Debug for SqldSigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SqldSigningKey(<redacted>)")
    }
}

impl SqldSigningKey {
    /// Gera uma chave aleatória nova (PKCS#8 v2 do ring).
    pub fn generate() -> Result<Self, SqldSignerError> {
        let rng = SystemRandom::new();
        let doc =
            Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| SqldSignerError::RandomFailed)?;
        Self::from_pkcs8(doc.as_ref())
    }

    fn from_pkcs8(pkcs8: &[u8]) -> Result<Self, SqldSignerError> {
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| SqldSignerError::KeyRejected)?;
        let public_key: [u8; PUBLIC_KEY_LEN] = pair
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| SqldSignerError::KeyRejected)?;
        Ok(Self {
            pkcs8: Zeroizing::new(pkcs8.to_vec()),
            public_key,
        })
    }

    /// Chave pública crua (32 bytes).
    pub fn public_key(&self) -> &[u8; PUBLIC_KEY_LEN] {
        &self.public_key
    }

    /// Componente `x` para `DecodingKey::from_ed_components`: base64
    /// URL-safe sem padding.
    pub fn public_key_b64url(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.public_key)
    }

    /// Sela o PKCS#8 sob a master dedicada, ligando dono + geração (AAD).
    pub fn seal(
        &self,
        master: &[u8; MASTER_KEY_LEN],
        owner: &SqldSignerOwner<'_>,
    ) -> Result<SealedSqldSigningKey, SqldSignerError> {
        let aad = build_aad(owner)?;
        let mut nonce_bytes = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| SqldSignerError::RandomFailed)?;
        let nonce = Nonce::try_from(&nonce_bytes[..]).map_err(|_| SqldSignerError::SealFailed)?;
        let cipher =
            ChaCha20Poly1305::new_from_slice(master).map_err(|_| SqldSignerError::SealFailed)?;
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: &self.pkcs8,
                    aad: &aad,
                },
            )
            .map_err(|_| SqldSignerError::SealFailed)?;
        Ok(SealedSqldSigningKey {
            nonce: nonce_bytes,
            ciphertext,
        })
    }

    /// Abre o envelope; falha igual para master, dono, namespace, geração,
    /// nonce ou ciphertext errados/adulterados.
    pub fn open(
        sealed: &SealedSqldSigningKey,
        master: &[u8; MASTER_KEY_LEN],
        owner: &SqldSignerOwner<'_>,
    ) -> Result<Self, SqldSignerError> {
        let aad = build_aad(owner)?;
        if sealed.ciphertext.len() <= TAG_LEN || sealed.ciphertext.len() > MAX_CIPHERTEXT_LEN {
            return Err(SqldSignerError::InvalidEnvelope);
        }
        let nonce =
            Nonce::try_from(&sealed.nonce[..]).map_err(|_| SqldSignerError::InvalidEnvelope)?;
        let cipher =
            ChaCha20Poly1305::new_from_slice(master).map_err(|_| SqldSignerError::OpenFailed)?;
        let pkcs8 = Zeroizing::new(
            cipher
                .decrypt(
                    &nonce,
                    Payload {
                        msg: &sealed.ciphertext,
                        aad: &aad,
                    },
                )
                .map_err(|_| SqldSignerError::OpenFailed)?,
        );
        Self::from_pkcs8(&pkcs8)
    }

    /// Assina o token escopado
    /// `{exp, p:{rw:{ns:[ns]}, ddl:{ns:[ns]}}}` (EdDSA). `now_unix >= 0`,
    /// `ttl_seconds` em `1..=86400`, `exp` com soma checada, namespace SQL
    /// não vazio, sem caracteres de controle (NUL/CR/LF incluídos) e
    /// limitado a [`MAX_NAME_LEN`].
    pub fn sign_scoped_token(
        &self,
        sql_namespace: &str,
        now_unix: i64,
        ttl_seconds: u64,
    ) -> Result<SqldScopedToken, SqldSignerError> {
        if !valid_text(sql_namespace) {
            return Err(SqldSignerError::InvalidNamespace);
        }
        if now_unix < 0 {
            return Err(SqldSignerError::InvalidTime);
        }
        if ttl_seconds == 0 || ttl_seconds > MAX_TTL_SECONDS {
            return Err(SqldSignerError::InvalidTtl);
        }
        let exp = (now_unix as u64)
            .checked_add(ttl_seconds)
            .filter(|exp| i64::try_from(*exp).is_ok())
            .ok_or(SqldSignerError::InvalidTime)?;
        let claims = json!({
            "exp": exp,
            "p": {
                "rw": { "ns": [sql_namespace] },
                "ddl": { "ns": [sql_namespace] },
            },
        });
        let key = EncodingKey::from_ed_der(&self.pkcs8);
        let jwt = jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), &claims, &key)
            .map_err(|_| SqldSignerError::SignFailed)?;
        Ok(SqldScopedToken(Zeroizing::new(jwt)))
    }
}

/// JWT assinado. Sem `Display`/`Serialize`; `Debug` redigido. A gravação no
/// cofre usa [`SqldScopedToken::expose_jwt`] explicitamente.
pub struct SqldScopedToken(Zeroizing<String>);

impl fmt::Debug for SqldScopedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SqldScopedToken(<redacted>)")
    }
}

impl SqldScopedToken {
    pub fn expose_jwt(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
    use serde_json::Value;

    const MASTER: [u8; MASTER_KEY_LEN] = [7u8; MASTER_KEY_LEN];
    const OTHER_MASTER: [u8; MASTER_KEY_LEN] = [9u8; MASTER_KEY_LEN];

    fn owner<'a>(worker: &'a str, ns: Option<&'a str>, generation: u64) -> SqldSignerOwner<'a> {
        SqldSignerOwner {
            worker,
            rbac_namespace: ns,
            generation,
        }
    }

    fn decoding(key: &SqldSigningKey) -> DecodingKey {
        DecodingKey::from_ed_components(&key.public_key_b64url()).expect("x component")
    }

    fn verify(key: &SqldSigningKey, jwt: &str, check_exp: bool) -> Result<Value, ()> {
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.validate_exp = check_exp;
        validation.leeway = 0;
        validation.required_spec_claims = ["exp".to_string()].into_iter().collect();
        decode::<Value>(jwt, &decoding(key), &validation)
            .map(|data| data.claims)
            .map_err(|_| ())
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[test]
    fn seal_open_roundtrip_keeps_public_key_and_signature_verifies() {
        let key = SqldSigningKey::generate().unwrap();
        let o = owner("app-a", Some("team"), 1);
        let sealed = key.seal(&MASTER, &o).unwrap();
        let opened = SqldSigningKey::open(&sealed, &MASTER, &o).unwrap();
        assert_eq!(key.public_key(), opened.public_key());
        let token = opened.sign_scoped_token("ns1", now(), 60).unwrap();
        // verificado com a chave pública da chave ORIGINAL (posse remota)
        assert!(verify(&key, token.expose_jwt(), true).is_ok());
    }

    #[test]
    fn generated_keys_are_distinct_and_cross_verification_fails() {
        let a = SqldSigningKey::generate().unwrap();
        let b = SqldSigningKey::generate().unwrap();
        assert_ne!(a.public_key(), b.public_key());
        let token = a.sign_scoped_token("ns", 1_000, 60).unwrap();
        assert!(verify(&b, token.expose_jwt(), false).is_err());
        assert!(verify(&a, token.expose_jwt(), false).is_ok());
    }

    #[test]
    fn two_seals_of_same_key_use_fresh_nonces() {
        let key = SqldSigningKey::generate().unwrap();
        let o = owner("app", None, 1);
        let s1 = key.seal(&MASTER, &o).unwrap();
        let s2 = key.seal(&MASTER, &o).unwrap();
        assert_ne!(s1.nonce(), s2.nonce());
        assert_ne!(s1.ciphertext(), s2.ciphertext());
    }

    #[test]
    fn open_fails_for_wrong_master_owner_namespace_and_generation() {
        let key = SqldSigningKey::generate().unwrap();
        let o = owner("app-a", Some("team"), 3);
        let sealed = key.seal(&MASTER, &o).unwrap();
        // control: owner correto abre
        assert!(SqldSigningKey::open(&sealed, &MASTER, &o).is_ok());
        let cases: [(&str, [u8; 32], SqldSignerOwner<'_>); 5] = [
            ("master", OTHER_MASTER, o),
            ("worker", MASTER, owner("app-b", Some("team"), 3)),
            ("namespace", MASTER, owner("app-a", Some("other"), 3)),
            ("namespace none", MASTER, owner("app-a", None, 3)),
            ("generation", MASTER, owner("app-a", Some("team"), 4)),
        ];
        for (label, master, who) in cases {
            assert_eq!(
                SqldSigningKey::open(&sealed, &master, &who).err(),
                Some(SqldSignerError::OpenFailed),
                "case {label}"
            );
        }
    }

    #[test]
    fn tampered_nonce_or_ciphertext_fails() {
        let key = SqldSigningKey::generate().unwrap();
        let o = owner("app", Some("ns"), 1);
        let sealed = key.seal(&MASTER, &o).unwrap();
        for i in [0, NONCE_LEN - 1] {
            let mut nonce = *sealed.nonce();
            nonce[i] ^= 1;
            let bad = SealedSqldSigningKey::from_parts(&nonce, sealed.ciphertext()).unwrap();
            assert_eq!(
                SqldSigningKey::open(&bad, &MASTER, &o).err(),
                Some(SqldSignerError::OpenFailed)
            );
        }
        let last = sealed.ciphertext().len() - 1;
        for i in [0, last / 2, last] {
            let mut ct = sealed.ciphertext().to_vec();
            ct[i] ^= 1;
            let bad = SealedSqldSigningKey::from_parts(sealed.nonce(), &ct).unwrap();
            assert_eq!(
                SqldSigningKey::open(&bad, &MASTER, &o).err(),
                Some(SqldSignerError::OpenFailed)
            );
        }
    }

    #[test]
    fn malformed_and_oversize_envelopes_fail_statically() {
        let nonce = [0u8; NONCE_LEN];
        let ct = [1u8; 40];
        for (label, n, c) in [
            ("short nonce", &nonce[..11], &ct[..]),
            ("long nonce", &[0u8; 13][..], &ct[..]),
            ("empty nonce", &[][..], &ct[..]),
            ("empty ciphertext", &nonce[..], &[][..]),
            ("tag-only ciphertext", &nonce[..], &ct[..TAG_LEN]),
            (
                "oversize ciphertext",
                &nonce[..],
                &vec![1u8; MAX_CIPHERTEXT_LEN + 1][..],
            ),
        ] {
            let err = SealedSqldSigningKey::from_parts(n, c).err();
            assert_eq!(err, Some(SqldSignerError::InvalidEnvelope), "case {label}");
        }
        // limite superior exato é aceito pelo parser (falha só ao abrir)
        let max = SealedSqldSigningKey::from_parts(&nonce, &vec![1u8; MAX_CIPHERTEXT_LEN]).unwrap();
        assert_eq!(
            SqldSigningKey::open(&max, &MASTER, &owner("a", None, 1)).err(),
            Some(SqldSignerError::OpenFailed)
        );
        // blob autenticado válido mas com plaintext que não é PKCS#8
        let aad = build_aad(&owner("a", None, 1)).unwrap();
        let cipher = ChaCha20Poly1305::new_from_slice(&MASTER).unwrap();
        let n = Nonce::try_from(&nonce[..]).unwrap();
        let ct = cipher
            .encrypt(
                &n,
                Payload {
                    msg: b"not pkcs8",
                    aad: &aad,
                },
            )
            .unwrap();
        let junk = SealedSqldSigningKey::from_parts(&nonce, &ct).unwrap();
        assert_eq!(
            SqldSigningKey::open(&junk, &MASTER, &owner("a", None, 1)).err(),
            Some(SqldSignerError::KeyRejected)
        );
    }

    #[test]
    fn aad_is_unambiguous() {
        let aad = |w, n, g| build_aad(&owner(w, n, g)).unwrap();
        assert_ne!(aad("ab", Some("c"), 1), aad("a", Some("bc"), 1));
        assert_ne!(aad("a", Some("bc"), 1), aad("ab", None, 1));
        // `Some("")` não é aceito: não pode colidir com `None`
        assert_eq!(
            build_aad(&owner("a", Some(""), 1)).err(),
            Some(SqldSignerError::InvalidRbacNamespace)
        );
    }

    #[test]
    fn aad_none_differs_from_some_and_binds_generation() {
        let w = "app";
        let none = build_aad(&owner(w, None, 1)).unwrap();
        let some = build_aad(&owner(w, Some("x"), 1)).unwrap();
        assert_ne!(none, some);
        assert_ne!(none, build_aad(&owner(w, None, 2)).unwrap());
        assert!(none.starts_with(AAD_PREFIX));
        // generation big-endian no fim
        assert_eq!(&none[none.len() - 8..], &1u64.to_be_bytes());
    }

    #[test]
    fn invalid_identities_are_rejected_without_echo() {
        let key = SqldSigningKey::generate().unwrap();
        let long = "x".repeat(MAX_NAME_LEN + 1);
        let max = "x".repeat(MAX_NAME_LEN);
        assert!(key
            .seal(&MASTER, &owner(&max, Some(&max), u64::MAX))
            .is_ok());
        for bad in ["", "a\0b", "a\nb", "a\rb", &long] {
            assert_eq!(
                key.seal(&MASTER, &owner(bad, None, 1)).err(),
                Some(SqldSignerError::InvalidWorker)
            );
            assert_eq!(
                key.seal(&MASTER, &owner("a", Some(bad), 1)).err(),
                Some(SqldSignerError::InvalidRbacNamespace)
            );
        }
        assert_eq!(
            key.seal(&MASTER, &owner("a", None, 0)).err(),
            Some(SqldSignerError::InvalidGeneration)
        );
        let sealed = key.seal(&MASTER, &owner("a", None, 1)).unwrap();
        assert_eq!(
            SqldSigningKey::open(&sealed, &MASTER, &owner("a", None, 0)).err(),
            Some(SqldSignerError::InvalidGeneration)
        );
        let msg = SqldSignerError::InvalidWorker.to_string();
        assert!(!msg.contains("a\0b"));
    }

    #[test]
    fn token_has_exact_claims_header_and_exp() {
        let key = SqldSigningKey::generate().unwrap();
        let token = key.sign_scoped_token("ns-1", 1_700_000_000, 3_600).unwrap();
        let header = decode_header(token.expose_jwt()).unwrap();
        assert_eq!(header.alg, Algorithm::EdDSA);
        let claims = verify(&key, token.expose_jwt(), false).unwrap();
        assert_eq!(
            claims,
            json!({
                "exp": 1_700_003_600u64,
                "p": { "rw": { "ns": ["ns-1"] }, "ddl": { "ns": ["ns-1"] } },
            })
        );
        let obj = claims.as_object().unwrap();
        for forbidden in ["id", "a", "fullaccess", "iat", "nbf"] {
            assert!(!obj.contains_key(forbidden), "claim {forbidden}");
        }
    }

    #[test]
    fn token_validates_against_the_real_clock_and_expired_fails() {
        let key = SqldSigningKey::generate().unwrap();
        let live = key.sign_scoped_token("ns", now(), 60).unwrap();
        assert!(verify(&key, live.expose_jwt(), true).is_ok());
        let expired = key.sign_scoped_token("ns", now() - 120, 60).unwrap();
        assert!(verify(&key, expired.expose_jwt(), true).is_err());
    }

    #[test]
    fn token_rejects_tampered_payload_and_wrong_algorithm_key() {
        let key = SqldSigningKey::generate().unwrap();
        let token = key.sign_scoped_token("ns", 1_000, 60).unwrap();
        let parts: Vec<&str> = token.expose_jwt().split('.').collect();
        let forged = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"exp":9999999999,"p":{"rw":{"ns":["x"]},"ddl":{"ns":["x"]}}}"#);
        let bad = format!("{}.{}.{}", parts[0], forged, parts[2]);
        assert!(verify(&key, &bad, false).is_err());
    }

    #[test]
    fn ttl_time_and_namespace_limits() {
        let key = SqldSigningKey::generate().unwrap();
        let sign = |ns: &str, now: i64, ttl: u64| key.sign_scoped_token(ns, now, ttl).err();
        assert_eq!(sign("ns", 0, 0), Some(SqldSignerError::InvalidTtl));
        assert_eq!(
            sign("ns", 0, MAX_TTL_SECONDS + 1),
            Some(SqldSignerError::InvalidTtl)
        );
        assert_eq!(sign("ns", 0, u64::MAX), Some(SqldSignerError::InvalidTtl));
        assert_eq!(sign("ns", -1, 60), Some(SqldSignerError::InvalidTime));
        assert_eq!(sign("ns", i64::MIN, 60), Some(SqldSignerError::InvalidTime));
        assert_eq!(sign("ns", i64::MAX, 1), Some(SqldSignerError::InvalidTime));
        assert_eq!(
            sign("ns", i64::MAX - 59, 60),
            Some(SqldSignerError::InvalidTime)
        );
        assert_eq!(sign("ns", i64::MAX - 60, 60), None); // exp == i64::MAX
        assert_eq!(sign("ns", 0, 1), None);
        assert_eq!(sign("ns", 0, MAX_TTL_SECONDS), None);
        let long = "n".repeat(MAX_NAME_LEN + 1);
        for bad in ["", "a\0", "a\r", "a\n", "a\r\nb", &long] {
            assert_eq!(sign(bad, 0, 60), Some(SqldSignerError::InvalidNamespace));
        }
        assert_eq!(sign(&"n".repeat(MAX_NAME_LEN), 0, 60), None);
    }

    #[test]
    fn debug_never_shows_key_material_or_token() {
        let key = SqldSigningKey::generate().unwrap();
        let o = owner("secret-worker", Some("secret-ns"), 5);
        let sealed = key.seal(&MASTER, &o).unwrap();
        let token = key.sign_scoped_token("ns", 1_000, 60).unwrap();
        let rendered = [
            format!("{key:?}"),
            format!("{sealed:?}"),
            format!("{token:?}"),
            format!("{o:?}"),
            format!("{:?}", SqldSignerError::OpenFailed),
        ]
        .join("|");
        let b64 = base64::engine::general_purpose::STANDARD;
        let pkcs8_b64 = b64.encode(&*key.pkcs8);
        let pkcs8_hex: String = key.pkcs8.iter().map(|b| format!("{b:02x}")).collect();
        let jwt_head = token.expose_jwt()[..20].to_string();
        // asserções só imprimem booleanos: nunca bytes privados ou JWT
        assert!(!rendered.contains(&pkcs8_b64));
        assert!(!rendered.contains(&pkcs8_hex));
        assert!(!rendered.contains(&jwt_head));
        assert!(!rendered.contains("secret-worker"));
        assert!(!rendered.contains("secret-ns"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn sealed_envelope_does_not_contain_plaintext_pkcs8() {
        let key = SqldSigningKey::generate().unwrap();
        let sealed = key.seal(&MASTER, &owner("app", None, 1)).unwrap();
        assert_eq!(sealed.ciphertext().len(), key.pkcs8.len() + TAG_LEN);
        let ct = sealed.ciphertext();
        let leaks = ct
            .windows(key.public_key().len())
            .any(|w| w == key.public_key().as_slice());
        assert!(!leaks);
        assert!(!ct
            .windows(16)
            .any(|w| key.pkcs8.windows(16).any(|p| p == w)));
    }
}
