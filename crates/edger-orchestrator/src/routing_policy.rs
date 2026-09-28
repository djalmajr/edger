//! Routing policy documents for one app name (story 25.01).
//!
//! No I/O. Absence of a policy keeps the current public route and
//! `defaultVersion`. `public` is a different mode from an empty allowlist.
//! A public document with no traffic is not a policy: that is absence.
//!
//! Bytes must enter through [`parse_routing_policy`]. `Deserialize` alone
//! does not reject duplicate JSON keys or slug/weight invariants.
//!
//! Weighted selection hashes `app name || NUL || cohort` with SHA-256 and
//! takes the first 8 bytes as a big-endian integer modulo 100. Ranges follow
//! the traffic array order. `edger_cohort` is not a credential: a client can
//! forge it and only chooses their own cohort. It carries no user, version,
//! or tenant, and must never become `x-tenant-id`.

use std::collections::{HashMap, HashSet};

use edger_core::CoreError;
use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize};

const MAX_TRAFFIC_VERSIONS: usize = 8;
const MAX_TENANT_SLUG_CHARS: usize = 63;
const MAX_JSON_DEPTH: u32 = 32;

/// In-memory policies keyed by full app name. Replaced as one value under
/// the manifest index write lock.
#[derive(Clone, Debug, Default)]
pub(crate) struct RoutingPolicyTable {
    by_name: HashMap<String, RoutingPolicy>,
}

impl RoutingPolicyTable {
    pub(crate) fn get(&self, name: &str) -> Option<RoutingPolicy> {
        self.by_name.get(name).cloned()
    }

    pub(crate) fn insert(&mut self, policy: RoutingPolicy) {
        self.by_name.insert(policy.name.clone(), policy);
    }

    pub(crate) fn remove(&mut self, name: &str) -> Option<RoutingPolicy> {
        self.by_name.remove(name)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &RoutingPolicy> {
        self.by_name.values()
    }
}

/// One app's tenant gate and optional weighted versions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutingPolicy {
    pub name: String,
    #[serde(deserialize_with = "deserialize_tenant_access")]
    pub tenant_access: TenantAccess,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic: Option<RoutingTraffic>,
}

/// `public` carries no tenant list. `allowlist` requires one or more slugs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum TenantAccess {
    Public,
    Allowlist { tenants: Vec<String> },
}

/// Weighted versions. Counts, uniqueness, and the sum are checked after parse.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingTraffic {
    pub versions: Vec<TrafficVersion>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrafficVersion {
    pub version: String,
    #[serde(deserialize_with = "deserialize_weight")]
    pub weight: u32,
}

impl RoutingPolicy {
    pub(crate) fn check_invariants(&self) -> Result<(), CoreError> {
        if self.name.is_empty() || self.name.contains('\0') {
            return Err(CoreError::validation(
                "routingPolicy.name",
                "name must be a non-empty app name",
            ));
        }
        match &self.tenant_access {
            TenantAccess::Public => {}
            TenantAccess::Allowlist { tenants } => {
                if tenants.is_empty() {
                    return Err(CoreError::validation(
                        "routingPolicy.tenantAccess.tenants",
                        "allowlist must name at least one tenant; an empty allowlist is not public access",
                    ));
                }
                let mut seen = HashSet::with_capacity(tenants.len());
                for slug in tenants {
                    if !is_tenant_slug(slug) {
                        return Err(CoreError::validation(
                            "routingPolicy.tenantAccess.tenants",
                            format!("tenant slug is invalid: {slug}"),
                        ));
                    }
                    if !seen.insert(slug.as_str()) {
                        return Err(CoreError::validation(
                            "routingPolicy.tenantAccess.tenants",
                            format!("duplicate tenant slug: {slug}"),
                        ));
                    }
                }
            }
        }
        match &self.traffic {
            None => {
                if matches!(self.tenant_access, TenantAccess::Public) {
                    return Err(CoreError::validation(
                        "routingPolicy",
                        "public access without traffic is the absence of a routing policy",
                    ));
                }
            }
            Some(traffic) => check_traffic(traffic)?,
        }
        Ok(())
    }
}

fn check_traffic(traffic: &RoutingTraffic) -> Result<(), CoreError> {
    if traffic.versions.is_empty() || traffic.versions.len() > MAX_TRAFFIC_VERSIONS {
        return Err(CoreError::validation(
            "routingPolicy.traffic.versions",
            format!("traffic must list between 1 and {MAX_TRAFFIC_VERSIONS} versions"),
        ));
    }
    let mut seen = HashSet::with_capacity(traffic.versions.len());
    let mut sum: u32 = 0;
    for version in &traffic.versions {
        if version.version.is_empty() || version.version.contains('\0') {
            return Err(CoreError::validation(
                "routingPolicy.traffic.versions",
                "traffic version must be a non-empty version string",
            ));
        }
        if !seen.insert(version.version.as_str()) {
            return Err(CoreError::validation(
                "routingPolicy.traffic.versions",
                format!("duplicate traffic version: {}", version.version),
            ));
        }
        if !(1..=100).contains(&version.weight) {
            return Err(CoreError::validation(
                "routingPolicy.traffic.versions",
                format!(
                    "weight must be an integer from 1 to 100, got {}",
                    version.weight
                ),
            ));
        }
        sum = sum.checked_add(version.weight).ok_or_else(|| {
            CoreError::validation(
                "routingPolicy.traffic.versions",
                "traffic weights must sum to 100",
            )
        })?;
    }
    if sum != 100 {
        return Err(CoreError::validation(
            "routingPolicy.traffic.versions",
            format!("traffic weights must sum to 100, got {sum}"),
        ));
    }
    Ok(())
}

/// Tenancit slug: `^[a-z0-9]+(?:-[a-z0-9]+)*$`, at most 63 characters.
/// No case folding and no trimming.
fn is_tenant_slug(slug: &str) -> bool {
    let len = slug.chars().count();
    if len == 0 || len > MAX_TENANT_SLUG_CHARS {
        return false;
    }
    slug.split('-').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

fn deserialize_tenant_access<'de, D>(deserializer: D) -> Result<TenantAccess, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let serde_json::Value::Object(map) = value else {
        return Err(D::Error::custom("tenantAccess must be an object"));
    };
    let mode = match map.get("mode") {
        Some(serde_json::Value::String(mode)) => mode.as_str(),
        _ => return Err(D::Error::custom("tenantAccess.mode is required")),
    };
    match mode {
        "public" => {
            if map.len() != 1 {
                return Err(D::Error::custom(
                    "unknown field in public tenantAccess; public is not an allowlist",
                ));
            }
            Ok(TenantAccess::Public)
        }
        "allowlist" => {
            for key in map.keys() {
                if key != "mode" && key != "tenants" {
                    return Err(D::Error::custom(format!(
                        "unknown field `{key}` in allowlist tenantAccess"
                    )));
                }
            }
            let Some(serde_json::Value::Array(items)) = map.get("tenants") else {
                return Err(D::Error::custom(
                    "allowlist tenants must be an array of slugs",
                ));
            };
            let mut tenants = Vec::with_capacity(items.len());
            for item in items {
                let serde_json::Value::String(slug) = item else {
                    return Err(D::Error::custom("tenant slug must be a string"));
                };
                tenants.push(slug.clone());
            }
            Ok(TenantAccess::Allowlist { tenants })
        }
        _ => Err(D::Error::custom(
            "tenantAccess.mode must be public or allowlist",
        )),
    }
}

fn deserialize_weight<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let serde_json::Value::Number(number) = value else {
        return Err(D::Error::custom("weight must be an integer from 1 to 100"));
    };
    let Some(weight) = number.as_u64() else {
        return Err(D::Error::custom("weight must be an integer from 1 to 100"));
    };
    u32::try_from(weight).map_err(|_| D::Error::custom("weight must be an integer from 1 to 100"))
}

/// Parse one policy document. Rejects duplicate keys, unknown fields, and
/// invariant failures. Does not look at the manifest index.
pub fn parse_routing_policy(bytes: &[u8]) -> Result<RoutingPolicy, CoreError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(CoreError::parse("routing policy document is empty"));
    }
    reject_duplicate_json_keys(bytes)?;
    let policy: RoutingPolicy = serde_json::from_slice(bytes).map_err(classify_json_error)?;
    policy.check_invariants()?;
    Ok(policy)
}

fn classify_json_error(error: serde_json::Error) -> CoreError {
    if error.is_syntax() || error.is_eof() || error.is_io() {
        CoreError::parse(format!("routing policy JSON is invalid: {error}"))
    } else {
        CoreError::validation("routingPolicy", error.to_string())
    }
}

fn reject_duplicate_json_keys(bytes: &[u8]) -> Result<(), CoreError> {
    let mut cursor = JsonCursor { bytes, index: 0 };
    cursor.skip_ws();
    cursor.scan_value(0)?;
    cursor.skip_ws();
    if cursor.index != cursor.bytes.len() {
        return Err(CoreError::parse("routing policy JSON has trailing data"));
    }
    Ok(())
}

struct JsonCursor<'a> {
    bytes: &'a [u8],
    index: usize,
}

impl JsonCursor<'_> {
    fn skip_ws(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.index += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    fn bump(&mut self) -> Result<u8, CoreError> {
        let byte = self
            .peek()
            .ok_or_else(|| CoreError::parse("routing policy JSON is truncated"))?;
        self.index += 1;
        Ok(byte)
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), CoreError> {
        if self.bump()? == byte {
            Ok(())
        } else {
            Err(CoreError::parse("routing policy JSON is invalid"))
        }
    }

    fn scan_value(&mut self, depth: u32) -> Result<(), CoreError> {
        if depth > MAX_JSON_DEPTH {
            return Err(CoreError::parse("routing policy JSON is too deep"));
        }
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.scan_object(depth),
            Some(b'[') => self.scan_array(depth),
            Some(b'"') => {
                self.parse_string()?;
                Ok(())
            }
            Some(b't') => self.consume_literal(b"true"),
            Some(b'f') => self.consume_literal(b"false"),
            Some(b'n') => self.consume_literal(b"null"),
            Some(b'-') | Some(b'0'..=b'9') => self.scan_number(),
            Some(_) => Err(CoreError::parse("routing policy JSON is invalid")),
            None => Err(CoreError::parse("routing policy JSON is truncated")),
        }
    }

    fn scan_object(&mut self, depth: u32) -> Result<(), CoreError> {
        self.expect(b'{')?;
        self.skip_ws();
        if self.eat(b'}') {
            return Ok(());
        }
        let mut keys = HashSet::new();
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            if !keys.insert(key) {
                return Err(CoreError::validation("routingPolicy", "duplicate JSON key"));
            }
            self.skip_ws();
            self.expect(b':')?;
            self.scan_value(depth + 1)?;
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b'}') {
                return Ok(());
            }
            return Err(CoreError::parse("routing policy JSON is invalid"));
        }
    }

    fn scan_array(&mut self, depth: u32) -> Result<(), CoreError> {
        self.expect(b'[')?;
        self.skip_ws();
        if self.eat(b']') {
            return Ok(());
        }
        loop {
            self.scan_value(depth + 1)?;
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b']') {
                return Ok(());
            }
            return Err(CoreError::parse("routing policy JSON is invalid"));
        }
    }

    fn consume_literal(&mut self, literal: &[u8]) -> Result<(), CoreError> {
        if self.bytes[self.index..].starts_with(literal) {
            self.index += literal.len();
            Ok(())
        } else {
            Err(CoreError::parse("routing policy JSON is invalid"))
        }
    }

    fn scan_number(&mut self) -> Result<(), CoreError> {
        if self.eat(b'-') && !self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            return Err(CoreError::parse("routing policy JSON is invalid"));
        }
        match self.peek() {
            Some(b'0') => self.index += 1,
            Some(b'1'..=b'9') => {
                while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    self.index += 1;
                }
            }
            _ => return Err(CoreError::parse("routing policy JSON is invalid")),
        }
        if self.eat(b'.') {
            if !self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                return Err(CoreError::parse("routing policy JSON is invalid"));
            }
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.index += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.index += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.index += 1;
            }
            if !self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                return Err(CoreError::parse("routing policy JSON is invalid"));
            }
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.index += 1;
            }
        }
        Ok(())
    }

    fn parse_string(&mut self) -> Result<String, CoreError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let byte = self.bump()?;
            match byte {
                b'"' => return Ok(out),
                b'\\' => match self.bump()? {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => out.push(self.decode_unicode_escape()?),
                    _ => {
                        return Err(CoreError::parse(
                            "routing policy JSON has an invalid escape",
                        ))
                    }
                },
                0x00..=0x1F => {
                    return Err(CoreError::parse(
                        "routing policy JSON has an unescaped control character",
                    ))
                }
                0x20..=0x7F => out.push(byte as char),
                _ => {
                    let width = utf8_width(byte).ok_or_else(|| {
                        CoreError::parse("routing policy JSON is not valid UTF-8")
                    })?;
                    let start = self.index - 1;
                    let end = start + width;
                    if end > self.bytes.len() {
                        return Err(CoreError::parse("routing policy JSON is truncated"));
                    }
                    let text = std::str::from_utf8(&self.bytes[start..end])
                        .map_err(|_| CoreError::parse("routing policy JSON is not valid UTF-8"))?;
                    out.push_str(text);
                    self.index = end;
                }
            }
        }
    }

    fn decode_unicode_escape(&mut self) -> Result<char, CoreError> {
        let unit = self.hex4()?;
        if (0xD800..=0xDBFF).contains(&unit) {
            if self.bump()? != b'\\' || self.bump()? != b'u' {
                return Err(CoreError::parse(
                    "routing policy JSON has an invalid unicode escape",
                ));
            }
            let low = self.hex4()?;
            if !(0xDC00..=0xDFFF).contains(&low) {
                return Err(CoreError::parse(
                    "routing policy JSON has an invalid unicode escape",
                ));
            }
            let code_point = 0x10000 + (((unit - 0xD800) as u32) << 10) + (low as u32 - 0xDC00);
            return char::from_u32(code_point).ok_or_else(|| {
                CoreError::parse("routing policy JSON has an invalid unicode escape")
            });
        }
        if (0xDC00..=0xDFFF).contains(&unit) {
            return Err(CoreError::parse(
                "routing policy JSON has an invalid unicode escape",
            ));
        }
        char::from_u32(u32::from(unit))
            .ok_or_else(|| CoreError::parse("routing policy JSON has an invalid unicode escape"))
    }

    fn hex4(&mut self) -> Result<u16, CoreError> {
        let mut value = 0u16;
        for _ in 0..4 {
            let digit = match self.bump()? {
                byte @ b'0'..=b'9' => byte - b'0',
                byte @ b'a'..=b'f' => byte - b'a' + 10,
                byte @ b'A'..=b'F' => byte - b'A' + 10,
                _ => {
                    return Err(CoreError::parse(
                        "routing policy JSON has an invalid unicode escape",
                    ))
                }
            };
            value = (value << 4) | u16::from(digit);
        }
        Ok(value)
    }
}

/// Opaque session cookie. Forging it only picks this client's cohort.
pub const COHORT_COOKIE_NAME: &str = "edger_cohort";

/// App names cannot contain NUL, so `app`+`ab` cannot collide with `appa`+`b`.
const COHORT_HASH_SEPARATOR: u8 = 0;

pub fn cohort_bucket(app_name: &str, cohort: &str) -> u8 {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(app_name.as_bytes());
    hasher.update([COHORT_HASH_SEPARATOR]);
    hasher.update(cohort.as_bytes());
    let digest = hasher.finalize();
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(prefix) % 100) as u8
}

/// Inclusive start, exclusive end, in policy array order. Bucket is `0..100`.
pub fn version_for_weight_bucket(traffic: &RoutingTraffic, bucket: u8) -> Option<&str> {
    let mut start = 0u32;
    for version in &traffic.versions {
        let end = start + version.weight;
        if u32::from(bucket) >= start && u32::from(bucket) < end {
            return Some(version.version.as_str());
        }
        start = end;
    }
    None
}

/// `(cohort, minted)`. A missing, malformed, or over-long value is replaced
/// with a new UUID v4. The value is not logged.
pub fn cohort_for_cookie_header(header: Option<&str>) -> (String, bool) {
    if let Some(header) = header {
        for part in header.split(';') {
            let Some((name, value)) = part.trim().split_once('=') else {
                continue;
            };
            if name.trim() != COHORT_COOKIE_NAME {
                continue;
            }
            if let Some(cohort) = accepted_cohort(value.trim()) {
                return (cohort, false);
            }
            return (new_cohort(), true);
        }
    }
    (new_cohort(), true)
}

pub fn cohort_set_cookie(cohort: &str) -> String {
    format!("{COHORT_COOKIE_NAME}={cohort}; Path=/; HttpOnly; SameSite=Lax")
}

fn accepted_cohort(value: &str) -> Option<String> {
    if value.len() != 36 {
        return None;
    }
    let id = uuid::Uuid::parse_str(value).ok()?;
    if id.get_version_num() != 4 {
        return None;
    }
    let canonical = id.as_hyphenated().to_string();
    if value != canonical {
        return None;
    }
    Some(canonical)
}

fn new_cohort() -> String {
    uuid::Uuid::new_v4().as_hyphenated().to_string()
}

fn utf8_width(byte: u8) -> Option<usize> {
    if byte & 0b1110_0000 == 0b1100_0000 {
        Some(2)
    } else if byte & 0b1111_0000 == 0b1110_0000 {
        Some(3)
    } else if byte & 0b1111_1000 == 0b1111_0000 {
        Some(4)
    } else {
        None
    }
}
