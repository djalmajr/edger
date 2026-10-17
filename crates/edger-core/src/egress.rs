//! Egress destination parsing and IP classification (EDG-38, S1).
//!
//! Pure vocabulary: the egress guard (S2) resolves a user-chosen destination
//! and must refuse SSRF — this module turns the destination host into a
//! normalized form and classifies every resolved IP. No I/O, no dependencies
//! beyond `std`.
//!
//! The blocked ranges are DATA (the `BLOCKED_V4` / `BLOCKED_V6` tables), not
//! scattered `if`s. Source: IANA IPv4 and IPv6 Special-Purpose Address
//! Registries (RFC 6890 framework), fetched 2026-10-17, plus the guard
//! additions (`224.0.0.0/4` multicast, `255.255.255.255/32` limited
//! broadcast, `ff00::/8` multicast). The registry changes over time; the
//! tables mirror the 2026-10-17 snapshot and the guard reads the
//! operator-configured additions (cluster/node CIDRs) through
//! `extra_blocked` at classification time.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// A destination host as it arrives from the worker (raw protocol text —
/// `CONNECT` line or absolute-form request), normalized before classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EgressHost {
    /// An IP literal in any accepted form, normalized to the `IpAddr`.
    /// IPv4 decimal/abbreviated/octal/hex forms (the `inet_aton` grammar)
    /// and IPv6 literals with or without brackets.
    Ip(IpAddr),
    /// A DNS name, lowercased and without the trailing dot (IDN arrives as
    /// ASCII punycode, so `xn--` is an ordinary label). S2 resolves it and
    /// classifies every resolved IP.
    Name(String),
    /// Not a valid destination host in any accepted form: empty, whitespace,
    /// `_`, `%`, `@`, an attached port, a control character, non-ASCII, a
    /// numeric form that does not fit an IPv4, or a malformed name.
    Invalid,
}

/// Reason an IP was refused by the egress classifier. The `Display` impl is
/// stable snake_case, for the operational event and the log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EgressBlock {
    /// `127.0.0.0/8` (v4) or `::1/128` (v6).
    Loopback,
    /// RFC 1918 private-use (v4) or RFC 4193 unique-local (v6).
    Private,
    /// `169.254.0.0/16` (v4, cloud metadata) or `fe80::/10` (v6).
    LinkLocal,
    /// `100.64.0.0/10` shared address space (RFC 6598).
    SharedCgnat,
    /// `0.0.0.0` (v4) or `::/128` (v6).
    Unspecified,
    /// `224.0.0.0/4` (v4) or `ff00::/8` (v6).
    Multicast,
    /// `255.255.255.255/32`.
    Broadcast,
    /// TEST-NET-1/2/3, `2001:db8::/32` and `3fff::/20`.
    Documentation,
    /// `198.18.0.0/15` (v4) or `2001:2::/48` (v6).
    Benchmarking,
    /// `240.0.0.0/4` (v4) and the other IANA special-purpose ranges that
    /// carry no more specific name.
    Reserved,
    /// `192.0.0.0/24` (v4) or `2001::/23` (v6) IETF protocol assignments.
    IetfProtocol,
    /// `2002::/16` (v6, whole range, no embedded-IPv4 extraction) and the
    /// `192.88.99.0/24`-family 6to4 relay anycast (v4).
    Tunnel6to4,
    /// `2001::/32` (v6, whole range, no embedded-IPv4 extraction).
    Teredo,
    /// An IPv6 address outside `2000::/3` with no more specific range name.
    NotGlobalUnicastV6,
    /// Inside `0.0.0.0/8` (v4, "this network").
    ThisNetwork,
    /// `64:ff9b::/96` / `64:ff9b:1::/48` (v6, RFC 6052/8215).
    Nat64,
    /// Inside an operator-configured CIDR (`extra_blocked`: cluster and
    /// node IP, read from the real configuration by S2).
    Configured,
}

impl fmt::Display for EgressBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            EgressBlock::Loopback => "loopback",
            EgressBlock::Private => "private",
            EgressBlock::LinkLocal => "link_local",
            EgressBlock::SharedCgnat => "shared_cgnat",
            EgressBlock::Unspecified => "unspecified",
            EgressBlock::Multicast => "multicast",
            EgressBlock::Broadcast => "broadcast",
            EgressBlock::Documentation => "documentation",
            EgressBlock::Benchmarking => "benchmarking",
            EgressBlock::Reserved => "reserved",
            EgressBlock::IetfProtocol => "ietf_protocol",
            EgressBlock::Tunnel6to4 => "tunnel_6to4",
            EgressBlock::Teredo => "teredo",
            EgressBlock::NotGlobalUnicastV6 => "not_global_unicast_v6",
            EgressBlock::ThisNetwork => "this_network",
            EgressBlock::Nat64 => "nat64",
            EgressBlock::Configured => "configured",
        };
        f.write_str(name)
    }
}

/// Parse a destination host into its normalized form.
///
/// IPv6 literals (bracketed or bare) and IPv4 in the `inet_aton` grammar
/// (1-4 dot-separated numeric labels: decimal, octal with a leading zero,
/// or `0x` hex; the last label occupies the remaining bytes) become
/// `EgressHost::Ip`. Valid DNS names become `EgressHost::Name` (lowercased,
/// trailing dot stripped). Everything else is `EgressHost::Invalid` — the
/// guard must never pass an unrecognized form to the resolver, because the
/// OS resolver (and the URL layer) normalize every numeric form to the
/// canonical IP before dialing (spike A6/A6b/A6c).
pub fn parse_egress_host(host: &str) -> EgressHost {
    // IPv6 literal: bracketed (`[::1]`) or bare (`::1`). The guard speaks a
    // raw protocol and receives both; a port attached to a bracketed
    // literal (`[::1]:443`) is invalid.
    if host.contains(':') {
        let candidate = host
            .strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
            .unwrap_or(host);
        return match candidate.parse::<Ipv6Addr>() {
            Ok(ip) => EgressHost::Ip(IpAddr::V6(ip)),
            Err(_) => EgressHost::Invalid,
        };
    }
    // IPv4 in the `inet_aton` grammar: every dot-separated label is numeric.
    // (An all-digit single label — `2130706433` — is a numeric form, not a
    // name, so the numeric path wins over the name rules here.)
    let labels = host.split('.');
    let all_numeric = labels.clone().all(is_numeric_label);
    if !host.is_empty() && all_numeric {
        return match parse_inet_aton(host) {
            Some(ip) => EgressHost::Ip(IpAddr::V4(ip)),
            None => EgressHost::Invalid,
        };
    }
    parse_dns_name(host)
}

/// A label of the numeric form: decimal, octal (leading zero) or `0x` hex
/// (`0x` with zero hex digits included — the `inet_aton` path then fails
/// and the host is `Invalid`).
fn is_numeric_label(label: &str) -> bool {
    if let Some(hex) = label
        .strip_prefix("0x")
        .or_else(|| label.strip_prefix("0X"))
    {
        return hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    if label.is_empty() {
        return false;
    }
    label.bytes().all(|b| b.is_ascii_digit())
}

/// The `inet_aton` value of a 1-4 part numeric host: each part is decimal,
/// octal (leading zero) or `0x` hex; every part but the last occupies one
/// byte and the last one occupies the remaining `5 - n` bytes (so `127.1`
/// and `127.0.1` are `127.0.0.1`). A value that does not fit is `None`.
fn parse_inet_aton(host: &str) -> Option<Ipv4Addr> {
    let parts: Vec<u32> = host
        .split('.')
        .map(parse_inet_aton_part)
        .collect::<Option<Vec<_>>>()?;
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let n = parts.len();
    for (i, value) in parts.iter().enumerate() {
        if i < n - 1 && *value > 0xff {
            return None;
        }
    }
    let last_width_bits = 8 * (5 - n);
    // Checked in `u64`: for a single part the width is 32 bits, and a `u32`
    // shifted by 32 would overflow.
    if parts[n - 1] as u64 >= 1u64 << last_width_bits {
        return None;
    }
    let mut value = 0u32;
    for (i, part) in parts.iter().enumerate() {
        // Every part but the last occupies exactly one byte, shifted from
        // the top; the last part fills the remaining bytes at position 0.
        let shift = if i == n - 1 { 0 } else { 8 * (3 - i) };
        value |= *part << shift;
    }
    Some(Ipv4Addr::from(value))
}

/// One numeric part of the `inet_aton` grammar. A leading zero (and more
/// than one character) is octal — `08` is not octal and fails; `0x` with no
/// hex digit fails; anything non-numeric fails.
fn parse_inet_aton_part(part: &str) -> Option<u32> {
    let (digits, radix) =
        if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
            if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            (hex, 16)
        } else if part.len() > 1 && part.starts_with('0') {
            if !part.bytes().all(|b| b.is_ascii_digit() && b <= b'7') {
                return None;
            }
            (part, 8)
        } else {
            if !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (part, 10)
        };
    u32::from_str_radix(digits, radix).ok()
}

/// A valid DNS name: labels of `[a-z0-9-]`, 1-63 characters, no leading or
/// trailing hyphen, at most 253 characters in total (after the trailing
/// dot is stripped), and a last label that is not entirely numeric.
fn parse_dns_name(host: &str) -> EgressHost {
    let lowered = host.to_ascii_lowercase();
    let name = lowered.strip_suffix('.').unwrap_or(&lowered);
    if name.is_empty() || name.len() > 253 {
        return EgressHost::Invalid;
    }
    // `name` is non-empty and has no trailing dot, so the last label always
    // exists (the empty-label check below also covers it).
    let last_label = name.rsplit('.').next().unwrap_or("");
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return EgressHost::Invalid;
        }
        if label.starts_with('-') || label.ends_with('-') {
            return EgressHost::Invalid;
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return EgressHost::Invalid;
        }
    }
    if last_label.bytes().all(|b| b.is_ascii_digit()) {
        return EgressHost::Invalid;
    }
    EgressHost::Name(name.to_string())
}

/// An operator-configured CIDR (cluster and node IP in S2) that the
/// classifier refuses with `EgressBlock::Configured`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EgressCidr {
    address: IpAddr,
    prefix_bits: u8,
}

impl EgressCidr {
    /// Parse `"10.42.0.0/16"`, `"167.235.206.217/32"` or `"2a01::/32"`; a
    /// bare IP is `/32` (v4) or `/128` (v6). The prefix address is canonical
    /// (`IpAddr` form — configuration values, not worker input).
    pub fn parse(value: &str) -> Option<EgressCidr> {
        let (address_text, bits_text) = match value.rsplit_once('/') {
            Some((address, bits)) => (address, Some(bits)),
            None => (value, None),
        };
        let address: IpAddr = address_text.parse().ok()?;
        let max_bits = match address {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        // A bare IP is the full /32 or /128; an explicit (even empty)
        // prefix must be a plain integer within the family's width.
        let (mut address, mut prefix_bits) = match bits_text {
            None => (address, max_bits),
            Some(bits) => {
                if bits.len() > 3 || !bits.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let bits: u8 = bits.parse().ok()?;
                if bits > max_bits {
                    return None;
                }
                (address, bits)
            }
        };
        // A mapped CIDR `::ffff:a.b.c.d/(96+n)` (n 0 to 32) is the same set
        // of addresses as `a.b.c.d/n`: canonicalize it so `contains` compares
        // the same family (a canonicalized destination is a v4). A v6 CIDR
        // with a prefix below 96 keeps its v6 form — the mapped bits are not
        // all fixed by the prefix there.
        if let IpAddr::V6(v6) = address {
            if let Some(mapped) = v6.to_ipv4_mapped().filter(|_| prefix_bits >= 96) {
                address = IpAddr::V4(mapped);
                prefix_bits -= 96;
            }
        }
        Some(EgressCidr {
            address,
            prefix_bits,
        })
    }

    /// Whether `ip` falls inside this CIDR (same family, prefix bits equal).
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.address, ip) {
            (IpAddr::V4(network), IpAddr::V4(ip)) => {
                prefix_bits_equal_u32(network.to_bits(), ip.to_bits(), self.prefix_bits)
            }
            (IpAddr::V6(network), IpAddr::V6(ip)) => prefix_bits_equal_u128(
                u128::from_be_bytes(network.octets()),
                u128::from_be_bytes(ip.octets()),
                self.prefix_bits,
            ),
            _ => false,
        }
    }
}

fn prefix_bits_equal_u32(value: u32, network: u32, bits: u8) -> bool {
    if bits == 0 {
        return true;
    }
    let mask = u32::MAX << (32 - bits);
    (value & mask) == (network & mask)
}

fn prefix_bits_equal_u128(value: u128, network: u128, bits: u8) -> bool {
    if bits == 0 {
        return true;
    }
    let mask = u128::MAX << (128 - bits);
    (value & mask) == (network & mask)
}

/// The v6 global-unicast prefix: the only v6 space the classifier accepts
/// before the specific-range table runs.
const GLOBAL_UNICAST_V6: u128 = 0x2000_0000_0000_0000_0000_0000_0000_0000;

/// Blocked IPv4 ranges: (network, prefix bits, reason). IANA IPv4
/// Special-Purpose Address Registry, fetched 2026-10-17, plus `224.0.0.0/4`
/// (multicast) and `255.255.255.255/32` (limited broadcast). Nested ranges
/// keep their own entry; the classifier picks the most specific match.
const BLOCKED_V4: &[(u32, u8, EgressBlock)] = &[
    (0x0000_0000, 32, EgressBlock::Unspecified), // 0.0.0.0/32 (RFC 9337)
    (0x0000_0000, 8, EgressBlock::ThisNetwork),  // 0.0.0.0/8 (RFC 791 §3.2)
    (0x0A00_0000, 8, EgressBlock::Private),      // 10.0.0.0/8 (RFC 1918)
    (0x6440_0000, 10, EgressBlock::SharedCgnat), // 100.64.0.0/10 (RFC 6598)
    (0x7F00_0000, 8, EgressBlock::Loopback),     // 127.0.0.0/8 (RFC 1122)
    (0xA9FE_0000, 16, EgressBlock::LinkLocal),   // 169.254.0.0/16 (RFC 3927)
    (0xAC10_0000, 12, EgressBlock::Private),     // 172.16.0.0/12 (RFC 1918)
    (0xC000_0000, 24, EgressBlock::IetfProtocol), // 192.0.0.0/24 (RFC 6890 §2.1)
    (0xC000_0000, 29, EgressBlock::IetfProtocol), // 192.0.0.0/29 (RFC 7335)
    (0xC000_0008, 32, EgressBlock::IetfProtocol), // 192.0.0.8/32 (RFC 7600)
    (0xC000_0009, 32, EgressBlock::IetfProtocol), // 192.0.0.9/32 (RFC 7723)
    (0xC000_000A, 32, EgressBlock::IetfProtocol), // 192.0.0.10/32 (RFC 8155)
    (0xC000_00AA, 32, EgressBlock::IetfProtocol), // 192.0.0.170/32 (RFC 8880)
    (0xC000_00AB, 32, EgressBlock::IetfProtocol), // 192.0.0.171/32 (RFC 7050)
    (0xC000_0200, 24, EgressBlock::Documentation), // 192.0.2.0/24 (RFC 5737)
    (0xC01F_C400, 24, EgressBlock::Reserved),    // 192.31.196.0/24 (RFC 7535)
    (0xC034_C100, 24, EgressBlock::Reserved),    // 192.52.193.0/24 (RFC 7450)
    (0xC058_6300, 24, EgressBlock::Tunnel6to4),  // 192.88.99.0/24 (RFC 7526)
    (0xC058_6302, 32, EgressBlock::Tunnel6to4),  // 192.88.99.2/32 (RFC 6751)
    (0xC0A8_0000, 16, EgressBlock::Private),     // 192.168.0.0/16 (RFC 1918)
    (0xC0AF_3000, 24, EgressBlock::Reserved),    // 192.175.48.0/24 (RFC 7534)
    (0xC612_0000, 15, EgressBlock::Benchmarking), // 198.18.0.0/15 (RFC 2544)
    (0xC633_6400, 24, EgressBlock::Documentation), // 198.51.100.0/24 (RFC 5737)
    (0xCB00_7100, 24, EgressBlock::Documentation), // 203.0.113.0/24 (RFC 5737)
    (0xE000_0000, 4, EgressBlock::Multicast),    // 224.0.0.0/4 (RFC 5771)
    (0xF000_0000, 4, EgressBlock::Reserved),     // 240.0.0.0/4 (RFC 1112 §4)
    (0xFFFF_FFFF, 32, EgressBlock::Broadcast),   // 255.255.255.255/32 (RFC 8190)
];

/// Blocked IPv6 ranges: (network, prefix bits, reason). IANA IPv6
/// Special-Purpose Address Registry, fetched 2026-10-17, plus `ff00::/8`
/// (multicast). `::ffff:0:0/96` (IPv4-mapped) is the single exception: it
/// is extracted to the embedded IPv4 and classified as IPv4 (done in
/// `classify_egress_ip`, not tabled here). `2002::/16` and `2001::/32` are
/// refused whole — no embedded-IPv4 extraction for 6to4 or Teredo.
const BLOCKED_V6: &[(u128, u8, EgressBlock)] = &[
    (
        0x0000_0000_0000_0000_0000_0000_0000_0000,
        128,
        EgressBlock::Unspecified,
    ), // ::/128 (RFC 4291)
    (
        0x0000_0000_0000_0000_0000_0000_0000_0001,
        128,
        EgressBlock::Loopback,
    ), // ::1/128 (RFC 4291)
    (
        0x0064_FF9B_0000_0000_0000_0000_0000_0000,
        96,
        EgressBlock::Nat64,
    ), // 64:ff9b::/96 (RFC 6052)
    (
        0x0064_FF9B_0001_0000_0000_0000_0000_0000,
        48,
        EgressBlock::Nat64,
    ), // 64:ff9b:1::/48 (RFC 8215)
    (
        0x0100_0000_0000_0000_0000_0000_0000_0000,
        64,
        EgressBlock::Reserved,
    ), // 100::/64 (RFC 6666)
    (
        0x0100_0000_0000_0001_0000_0000_0000_0000,
        64,
        EgressBlock::Reserved,
    ), // 100:0:0:1::/64 (RFC 9780)
    (
        0x5F00_0000_0000_0000_0000_0000_0000_0000,
        16,
        EgressBlock::Reserved,
    ), // 5f00::/16 (RFC 9602)
    (
        0xFC00_0000_0000_0000_0000_0000_0000_0000,
        7,
        EgressBlock::Private,
    ), // fc00::/7 (RFC 4193)
    (
        0xFE80_0000_0000_0000_0000_0000_0000_0000,
        10,
        EgressBlock::LinkLocal,
    ), // fe80::/10 (RFC 4291)
    (
        0xFF00_0000_0000_0000_0000_0000_0000_0000,
        8,
        EgressBlock::Multicast,
    ), // ff00::/8 (RFC 4291)
    // Inside 2000::/3 — refused with their own reason instead of the global
    // unicast pass-through.
    (
        0x2001_0000_0000_0000_0000_0000_0000_0000,
        23,
        EgressBlock::IetfProtocol,
    ), // 2001::/23 (RFC 2928)
    (
        0x2001_0000_0000_0000_0000_0000_0000_0000,
        32,
        EgressBlock::Teredo,
    ), // 2001::/32 (RFC 4380)
    (
        0x2001_0001_0000_0000_0000_0000_0000_0001,
        128,
        EgressBlock::IetfProtocol,
    ), // 2001:1::1/128 (RFC 7723)
    (
        0x2001_0001_0000_0000_0000_0000_0000_0002,
        128,
        EgressBlock::IetfProtocol,
    ), // 2001:1::2/128 (RFC 8155)
    (
        0x2001_0001_0000_0000_0000_0000_0000_0003,
        128,
        EgressBlock::IetfProtocol,
    ), // 2001:1::3/128 (RFC 9665)
    (
        0x2001_0002_0000_0000_0000_0000_0000_0000,
        48,
        EgressBlock::Benchmarking,
    ), // 2001:2::/48 (RFC 5180)
    (
        0x2001_0003_0000_0000_0000_0000_0000_0000,
        32,
        EgressBlock::Reserved,
    ), // 2001:3::/32 (RFC 7450)
    (
        0x2001_0004_0112_0000_0000_0000_0000_0000,
        48,
        EgressBlock::Reserved,
    ), // 2001:4:112::/48 (RFC 7535)
    (
        0x2001_0010_0000_0000_0000_0000_0000_0000,
        28,
        EgressBlock::Reserved,
    ), // 2001:10::/28 (RFC 4843)
    (
        0x2001_0020_0000_0000_0000_0000_0000_0000,
        28,
        EgressBlock::Reserved,
    ), // 2001:20::/28 (RFC 7343)
    (
        0x2001_0030_0000_0000_0000_0000_0000_0000,
        28,
        EgressBlock::Reserved,
    ), // 2001:30::/28 (RFC 9374)
    (
        0x2001_0DB8_0000_0000_0000_0000_0000_0000,
        32,
        EgressBlock::Documentation,
    ), // 2001:db8::/32 (RFC 3849)
    (
        0x2002_0000_0000_0000_0000_0000_0000_0000,
        16,
        EgressBlock::Tunnel6to4,
    ), // 2002::/16 (RFC 3056)
    (
        0x2620_004F_8000_0000_0000_0000_0000_0000,
        48,
        EgressBlock::Reserved,
    ), // 2620:4f:8000::/48 (RFC 7534)
    (
        0x3FFF_0000_0000_0000_0000_0000_0000_0000,
        20,
        EgressBlock::Documentation,
    ), // 3fff::/20 (RFC 9637)
];

/// Classify a resolved destination IP for the egress guard.
///
/// The IP is canonicalized FIRST: an IPv4-mapped v6 (`::ffff:0:0/96`) becomes
/// the IPv4 address, and only then are the operator-configured CIDRs
/// (`extra_blocked`, `EgressBlock::Configured`) and the tables tested. The
/// configured CIDRs are tested against BOTH the original destination and the
/// canonicalized one: a v4 CIDR catches the mapped form through the
/// canonical one, and a v6 CIDR that stays v6 at parse (prefix below 96,
/// e.g. `::/0`) and covers the mapped block catches it through the
/// original. (A mapped CIDR written as `::ffff:a.b.c.d/(96+n)` — including
/// `::ffff:0:0/96` — is canonicalized to v4 by `EgressCidr::parse` itself,
/// so it catches the mapped form through the canonical one too.) 6to4 and
/// Teredo are refused whole, with no embedded-IPv4 extraction. For the rest,
/// the most specific table match wins; an IPv6 address inside `2000::/3`
/// with no table match is accepted (global unicast), everything outside is
/// refused.
pub fn classify_egress_ip(ip: IpAddr, extra_blocked: &[EgressCidr]) -> Result<(), EgressBlock> {
    let canonical = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        other => other,
    };
    for cidr in extra_blocked {
        if cidr.contains(ip) || cidr.contains(canonical) {
            return Err(EgressBlock::Configured);
        }
    }
    match canonical {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

fn classify_v4(ip: Ipv4Addr) -> Result<(), EgressBlock> {
    match most_specific(BLOCKED_V4, ip.to_bits()) {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

fn classify_v6(ip: Ipv6Addr) -> Result<(), EgressBlock> {
    let bits = u128::from_be_bytes(ip.octets());
    if prefix_bits_equal_u128(bits, GLOBAL_UNICAST_V6, 3) {
        // Only global unicast is accepted; the specific ranges inside it
        // keep their own reason.
        return match most_specific_v6(BLOCKED_V6, bits) {
            Some(reason) => Err(reason),
            None => Ok(()),
        };
    }
    match most_specific_v6(BLOCKED_V6, bits) {
        Some(reason) => Err(reason),
        None => Err(EgressBlock::NotGlobalUnicastV6),
    }
}

/// The reason of the most specific (longest-prefix) matching table entry.
fn most_specific(entries: &[(u32, u8, EgressBlock)], bits: u32) -> Option<EgressBlock> {
    let mut best: Option<(u8, EgressBlock)> = None;
    for &(network, net_bits, reason) in entries {
        if prefix_bits_equal_u32(bits, network, net_bits)
            && best
                .as_ref()
                .is_none_or(|&(match_bits, _)| net_bits >= match_bits)
        {
            best = Some((net_bits, reason));
        }
    }
    best.map(|(_, reason)| reason)
}

fn most_specific_v6(entries: &[(u128, u8, EgressBlock)], bits: u128) -> Option<EgressBlock> {
    let mut best: Option<(u8, EgressBlock)> = None;
    for &(network, net_bits, reason) in entries {
        if prefix_bits_equal_u128(bits, network, net_bits)
            && best
                .as_ref()
                .is_none_or(|&(match_bits, _)| net_bits >= match_bits)
        {
            best = Some((net_bits, reason));
        }
    }
    best.map(|(_, reason)| reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(text: &str) -> IpAddr {
        text.parse::<Ipv4Addr>().unwrap().into()
    }

    fn v6(text: &str) -> IpAddr {
        text.parse::<Ipv6Addr>().unwrap().into()
    }

    fn assert_blocked(host: &str, reason: EgressBlock) {
        let parsed = parse_egress_host(host);
        let ip = match parsed {
            EgressHost::Ip(ip) => ip,
            other => panic!("{host:?} must parse to an IP literal, got {other:?}"),
        };
        let err = classify_egress_ip(ip, &[]).unwrap_err();
        assert_eq!(err, reason, "{host:?} ({ip})");
    }

    fn assert_allowed(host: &str) {
        let parsed = parse_egress_host(host);
        let ip = match parsed {
            EgressHost::Ip(ip) => ip,
            other => panic!("{host:?} must parse to an IP literal, got {other:?}"),
        };
        assert!(
            classify_egress_ip(ip, &[]).is_ok(),
            "{host:?} ({ip}) must be accepted"
        );
    }

    // --- host parsing: loopback forms ----------------------------------------------------------------

    /// Every loopback form the OS resolver normalizes to the real loopback
    /// (spike A6/A6c) parses to the canonical IP and is refused as loopback.
    #[test]
    fn every_loopback_form_is_refused() {
        for host in [
            "127.0.0.1",
            "2130706433",
            "0177.0.0.1",
            "0x7f.0.0.1",
            "127.1",
            "127.0.1",
            "[::1]",
            "::ffff:127.0.0.1",
            "[::ffff:7f00:1]",
        ] {
            assert_blocked(host, EgressBlock::Loopback);
        }
        // The numeric forms normalize to the canonical 127.0.0.1.
        for host in [
            "127.0.0.1",
            "2130706433",
            "0177.0.0.1",
            "0x7f.0.0.1",
            "127.1",
            "127.0.1",
        ] {
            assert_eq!(
                parse_egress_host(host),
                EgressHost::Ip(v4("127.0.0.1")),
                "{host:?}"
            );
        }
        assert_eq!(parse_egress_host("[::1]"), EgressHost::Ip(v6("::1")));
        assert_eq!(
            parse_egress_host("::ffff:127.0.0.1"),
            EgressHost::Ip(v6("::ffff:127.0.0.1"))
        );
        assert_eq!(
            parse_egress_host("[::ffff:7f00:1]"),
            EgressHost::Ip(v6("::ffff:127.0.0.1")),
            "the hex dotted form normalizes to the same mapped address"
        );
    }

    // --- host parsing: other refused classes ---------------------------------------------------------

    /// Cloud metadata (link-local) in the dotted and the single-decimal form.
    #[test]
    fn metadata_ip_is_refused_in_every_form() {
        assert_blocked("169.254.169.254", EgressBlock::LinkLocal);
        assert_blocked("2852039166", EgressBlock::LinkLocal);
        assert_eq!(
            parse_egress_host("2852039166"),
            EgressHost::Ip(v4("169.254.169.254"))
        );
    }

    /// RFC 1918 private ranges and the CGNAT shared range.
    #[test]
    fn private_and_cgnat_ips_are_refused() {
        assert_blocked("10.0.0.1", EgressBlock::Private);
        assert_blocked("10.42.0.5", EgressBlock::Private);
        assert_blocked("10.43.0.1", EgressBlock::Private);
        assert_blocked("172.16.0.1", EgressBlock::Private);
        assert_blocked("192.168.1.1", EgressBlock::Private);
        assert_blocked("100.64.0.1", EgressBlock::SharedCgnat);
    }

    /// 6to4 and Teredo are refused whole (no embedded-IPv4 extraction),
    /// along with the other non-global v6 classes.
    #[test]
    fn tunnel_and_non_global_v6_are_refused() {
        assert_blocked("2002:7f00:1::", EgressBlock::Tunnel6to4);
        assert_blocked("2002:a9fe:a9fe::", EgressBlock::Tunnel6to4);
        assert_blocked("2001:0:102:304:203:506:101:102", EgressBlock::Teredo);
        assert_blocked("fc00::1", EgressBlock::Private);
        assert_blocked("fe80::1", EgressBlock::LinkLocal);
        assert_blocked("ff02::1", EgressBlock::Multicast);
        assert_blocked("::", EgressBlock::Unspecified);
        assert_blocked("64:ff9b::7f00:1", EgressBlock::Nat64);
        assert_blocked("2001:db8::1", EgressBlock::Documentation);
    }

    /// The whole IANA v4 table is covered: one address inside each range is
    /// refused with the range's reason. (This is the test that catches a
    /// row removed from `BLOCKED_V4`.)
    #[test]
    fn every_v4_table_range_is_refused() {
        for (host, reason) in [
            ("0.0.0.0", EgressBlock::Unspecified), // the /32 is more specific than the /8
            ("0.0.0.1", EgressBlock::ThisNetwork),
            ("0.255.1.1", EgressBlock::ThisNetwork),
            ("10.255.0.1", EgressBlock::Private),
            ("100.64.0.1", EgressBlock::SharedCgnat),
            ("100.127.255.255", EgressBlock::SharedCgnat),
            ("127.255.1.1", EgressBlock::Loopback),
            ("169.254.0.1", EgressBlock::LinkLocal),
            ("172.16.0.1", EgressBlock::Private),
            ("172.31.255.255", EgressBlock::Private),
            ("192.0.0.1", EgressBlock::IetfProtocol),
            ("192.0.0.7", EgressBlock::IetfProtocol), // inside 192.0.0.0/29 only
            ("192.0.0.8", EgressBlock::IetfProtocol),
            ("192.0.0.11", EgressBlock::IetfProtocol), // /24 but outside the /29 and the /32s
            ("192.0.0.170", EgressBlock::IetfProtocol),
            ("192.0.2.1", EgressBlock::Documentation),
            ("192.31.196.1", EgressBlock::Reserved),
            ("192.52.193.1", EgressBlock::Reserved),
            ("192.88.99.1", EgressBlock::Tunnel6to4),
            ("192.88.99.2", EgressBlock::Tunnel6to4),
            ("192.88.99.3", EgressBlock::Tunnel6to4),
            ("192.168.0.1", EgressBlock::Private),
            ("192.175.48.1", EgressBlock::Reserved),
            ("198.18.0.1", EgressBlock::Benchmarking),
            ("198.19.255.255", EgressBlock::Benchmarking),
            ("198.51.100.1", EgressBlock::Documentation),
            ("203.0.113.1", EgressBlock::Documentation),
            ("224.0.0.1", EgressBlock::Multicast),
            ("239.255.255.255", EgressBlock::Multicast),
            ("240.0.0.1", EgressBlock::Reserved),
            ("254.255.255.255", EgressBlock::Reserved),
            ("255.255.255.255", EgressBlock::Broadcast),
        ] {
            assert_blocked(host, reason);
        }
        // Controls: the first addresses just outside the CGNAT and the RFC
        // 1918 v2 ranges are public and accepted (the ranges are not wider
        // than their prefixes).
        assert!(classify_egress_ip(v4("100.128.0.1"), &[]).is_ok());
        assert!(classify_egress_ip(v4("172.32.0.1"), &[]).is_ok());
    }

    /// The whole IANA v6 table is covered: one address inside each range is
    /// refused with the range's reason, and an address just outside the
    /// special ranges of `2000::/3` is accepted. (Catches a row removed from
    /// `BLOCKED_V6`.)
    #[test]
    fn every_v6_table_range_is_refused() {
        for (host, reason) in [
            ("::", EgressBlock::Unspecified),
            ("::1", EgressBlock::Loopback),
            ("64:ff9b::1", EgressBlock::Nat64),
            ("64:ff9b:1::1", EgressBlock::Nat64),
            ("100::1", EgressBlock::Reserved),
            ("100:0:0:1::1", EgressBlock::Reserved),
            ("5f00::1", EgressBlock::Reserved),
            ("fc00::1", EgressBlock::Private),
            ("fdff:ffff::1", EgressBlock::Private),
            ("fe80::1", EgressBlock::LinkLocal),
            ("febf:ffff:ffff::1", EgressBlock::LinkLocal),
            ("ff02::1", EgressBlock::Multicast),
            ("2001::1", EgressBlock::Teredo), // inside 2001::/32 (the most specific)
            ("2001:0:ffff:ffff::1", EgressBlock::Teredo),
            ("2001:1::1", EgressBlock::IetfProtocol), // the /128 anycast (its own entry)
            ("2001:1::4", EgressBlock::IetfProtocol), // outside 2001::/32: the /23 reason
            ("2001:2::1", EgressBlock::Benchmarking),
            ("2001:3::1", EgressBlock::Reserved),
            ("2001:4:112::1", EgressBlock::Reserved),
            ("2001:10::1", EgressBlock::Reserved),
            ("2001:20::1", EgressBlock::Reserved),
            ("2001:30::1", EgressBlock::Reserved),
            ("2001:db8::1", EgressBlock::Documentation),
            ("2002::1", EgressBlock::Tunnel6to4),
            ("2002:ffff::1", EgressBlock::Tunnel6to4),
            ("2620:4f:8000::1", EgressBlock::Reserved),
            ("3fff::1", EgressBlock::Documentation),
            ("3fff:0fff::1", EgressBlock::Documentation),
        ] {
            assert_blocked(host, reason);
        }
        // Outside the special ranges, 2000::/3 is accepted.
        assert_allowed("2000::1");
        assert_allowed("2007:ffff::1");
        // Outside 2000::/3 with no named range: refused as not-global.
        let err = classify_egress_ip(v6("1::1"), &[]).unwrap_err();
        assert_eq!(err, EgressBlock::NotGlobalUnicastV6);
    }

    /// The mapped range is the only extraction: the embedded IPv4 is
    /// classified as IPv4 in both the dotted-quad and the `x:x` spelling.
    #[test]
    fn ipv4_mapped_is_extracted_and_classified_as_v4() {
        assert_blocked("::ffff:10.0.0.1", EgressBlock::Private);
        assert_blocked("[::ffff:a9fe:a9fe]", EgressBlock::LinkLocal);
        assert_blocked("::ffff:203.0.113.7", EgressBlock::Documentation);
        // Controls: the embedded public v4 passes in both spellings.
        assert!(classify_egress_ip(v6("::ffff:1.1.1.1"), &[]).is_ok());
        assert_allowed("[::ffff:93.184.216.34]");
    }

    // --- host parsing: invalid forms -----------------------------------------------------------------

    /// Forms that do not fit an IPv4 (or are not names) are `Invalid` —
    /// the guard must never forward them to the resolver.
    #[test]
    fn out_of_range_numeric_forms_are_invalid() {
        for host in [
            "256.1.1.1",
            "1.2.3.4.5",
            "08.1.1.1",
            "0x1ffffffff",
            "0x",
            "1.2.3.4 ",
            "127.0.0.1:443",
        ] {
            assert_eq!(parse_egress_host(host), EgressHost::Invalid, "{host:?}");
        }
    }

    /// Malformed names, and the hostile classes (whitespace, `_`, `%`, `@`,
    /// control characters, non-ASCII), are `Invalid`.
    #[test]
    fn malformed_names_are_invalid() {
        let long64 = "a".repeat(64);
        let trailing_hyphen = "a.b-".to_string();
        let hosts: Vec<&str> = vec![
            "exa mple.com",
            "a..b",
            "-a.com",
            "a.123",
            "",
            &long64,
            "a..com",
            "a_b.com",
            "a%b.com",
            "a@b.com",
            "a\tb.com",
            "exämple.com",
            &trailing_hyphen,
        ];
        for host in hosts {
            assert_eq!(parse_egress_host(host), EgressHost::Invalid, "{host:?}");
        }
    }

    /// Valid public IPs and names are accepted; names are lowercased and the
    /// trailing dot is stripped. `xn--` punycode is an ordinary label.
    #[test]
    fn public_ips_and_valid_names_are_accepted() {
        assert_allowed("93.184.216.34");
        assert_allowed("1.1.1.1");
        assert_allowed("2606:4700:4700::1111");
        assert_allowed("2a01:4f8::1");
        assert_eq!(
            parse_egress_host("Example.COM."),
            EgressHost::Name("example.com".into())
        );
        assert_eq!(
            parse_egress_host("xn--nxasmq6b.example"),
            EgressHost::Name("xn--nxasmq6b.example".into())
        );
        assert_eq!(
            parse_egress_host("a.b.c"),
            EgressHost::Name("a.b.c".into()),
            "a multi-label name whose last label is not all numeric"
        );
        // A 63-character label and the 253-character total are the boundary.
        let label63 = "a".repeat(63);
        assert_eq!(
            parse_egress_host(&label63),
            EgressHost::Name(label63.clone())
        );
        assert_eq!(
            parse_egress_host(&(label63.clone() + ".com")),
            EgressHost::Name(format!("{}.com", label63))
        );
        let over_total = vec!["a".repeat(63); 4].join(".");
        assert_eq!(
            parse_egress_host(&over_total),
            EgressHost::Invalid,
            "255 characters total exceeds the 253 limit"
        );
    }

    // --- EgressCidr -----------------------------------------------------------------------------------

    #[test]
    fn cidr_parse_and_contains() {
        let v4 = EgressCidr::parse("10.42.0.0/16").expect("parses");
        assert!(v4.contains(v4addr("10.42.0.5")));
        assert!(v4.contains(v4addr("10.42.255.255")));
        assert!(!v4.contains(v4addr("10.43.0.1")));
        assert!(!v4.contains(v6addr("::1")));
        let single = EgressCidr::parse("167.235.206.217/32").expect("parses");
        assert!(single.contains(v4addr("167.235.206.217")));
        assert!(!single.contains(v4addr("167.235.206.218")));
        assert!(!single.contains(v6addr("::ffff:167.235.206.217")));
        // a bare IP is /32 (v4) or /128 (v6).
        assert!(EgressCidr::parse("10.42.0.5")
            .unwrap()
            .contains(v4addr("10.42.0.5")));
        assert!(!EgressCidr::parse("10.42.0.5")
            .unwrap()
            .contains(v4addr("10.42.0.6")));
        assert!(EgressCidr::parse("::1").unwrap().contains(v6addr("::1")));
        assert!(!EgressCidr::parse("::1").unwrap().contains(v6addr("::2")));
        let v6 = EgressCidr::parse("2a01::/32").expect("parses");
        assert!(v6.contains(v6addr("2a01::1")));
        assert!(
            !v6.contains(v6addr("2a01:4f8::1")),
            "a /32 prefix is not wider"
        );
        // Host bits in the prefix do not matter (only the prefix bits match).
        assert!(EgressCidr::parse("10.42.1.5/16")
            .unwrap()
            .contains(v4addr("10.42.0.5")));
        // Invalid forms.
        for bad in [
            "10.42.0.0/33",
            "2a01::/129",
            "",
            "10.42.0.0/",
            "10.42.0.0/16/8",
            "10.42.0.0/-1",
            "not-an-ip/16",
            "10.42.0.0/16 ",
        ] {
            assert!(EgressCidr::parse(bad).is_none(), "{bad:?}");
        }
    }

    /// A mapped CIDR `::ffff:a.b.c.d/(96+n)` (n 0 to 32) canonicalizes to
    /// `a.b.c.d/n` at parse (a canonicalized v4 destination then matches);
    /// a v6 CIDR with a prefix below 96 keeps its v6 form, and a
    /// non-mapped v6 CIDR never becomes v4.
    #[test]
    fn mapped_cidrs_canonicalize_to_v4() {
        // /96 -> /0: every v4.
        let all = EgressCidr::parse("::ffff:0.0.0.0/96").unwrap();
        assert!(all.contains(v4addr("1.2.3.4")));
        assert!(all.contains(v4addr("255.255.255.255")));
        assert!(!all.contains(v6addr("::1")));
        // /104 -> /8: 10.x inside, 11.x outside.
        let ten = EgressCidr::parse("::ffff:10.0.0.0/104").unwrap();
        assert!(ten.contains(v4addr("10.1.2.3")));
        assert!(!ten.contains(v4addr("11.0.0.1")));
        // /128 -> /32: exactly that v4.
        let single = EgressCidr::parse("::ffff:167.235.206.217/128").unwrap();
        assert!(single.contains(v4addr("167.235.206.217")));
        assert!(!single.contains(v4addr("167.235.206.218")));
        // Prefix below 96: the mapped bits are not all fixed, so the CIDR
        // stays v6 (a v4 destination is a different family and misses).
        let wide = EgressCidr::parse("::ffff:10.0.0.0/95").unwrap();
        assert!(wide.contains(v6addr("::ffff:10.0.0.1")));
        assert!(!wide.contains(v4addr("10.0.0.1")));
        // A non-mapped v6 CIDR with a prefix >= 96 never becomes v4.
        let not_mapped = EgressCidr::parse("::1/100").unwrap();
        assert!(not_mapped.contains(v6addr("::1")));
        assert!(!not_mapped.contains(v4addr("0.0.0.1")));
    }

    fn v4addr(text: &str) -> IpAddr {
        text.parse::<Ipv4Addr>().unwrap().into()
    }

    fn v6addr(text: &str) -> IpAddr {
        text.parse::<Ipv6Addr>().unwrap().into()
    }

    // --- extra_blocked (Configured) ------------------------------------------------------------------

    /// An operator-configured CIDR wins with the `Configured` reason, in
    /// v4 and v6 — and only the configured range is added to the refusal.
    #[test]
    fn configured_cidrs_are_refused_with_configured() {
        let node = EgressCidr::parse("167.235.206.217/32").unwrap();
        let cluster = EgressCidr::parse("2a01:4f8::/48").unwrap();
        let extra = [node, cluster];
        assert_eq!(
            classify_egress_ip(v4addr("167.235.206.217"), &extra).unwrap_err(),
            EgressBlock::Configured
        );
        assert_eq!(
            classify_egress_ip(v6addr("2a01:4f8::1"), &extra).unwrap_err(),
            EgressBlock::Configured,
            "2a01:4f8::1 is a public v6 that the configured 2a01:4f8::/48 must block"
        );
        // The configured CIDRs do not widen the refusal: neighbors pass.
        assert!(classify_egress_ip(v4addr("167.235.206.218"), &extra).is_ok());
        assert!(classify_egress_ip(v6addr("2a01:4f9::1"), &extra).is_ok());
        // A mapped form of the node IP must be caught by the v4 configured
        // CIDR too: the classifier canonicalizes before testing
        // `extra_blocked` (an attacker-controlled AAAA could answer
        // `::ffff:<node ip>`), and a mapped CIDR is canonicalized the same
        // way at parse.
        assert_eq!(
            classify_egress_ip(v6addr("::ffff:167.235.206.217"), &[node]).unwrap_err(),
            EgressBlock::Configured
        );
        let bracketed = parse_egress_host("[::ffff:a7eb:ced9]");
        let bracketed_ip = match bracketed {
            EgressHost::Ip(ip) => ip,
            other => panic!("[::ffff:a7eb:ced9] must parse to an IP, got {other:?}"),
        };
        assert_eq!(
            bracketed_ip,
            v6addr("::ffff:167.235.206.217"),
            "a7eb:ced9 is the x:x spelling of 167.235.206.217"
        );
        assert_eq!(
            classify_egress_ip(bracketed_ip, &[node]).unwrap_err(),
            EgressBlock::Configured,
            "the bracketed x:x spelling resolves to the same mapped IP"
        );
        let mapped_cluster = EgressCidr::parse("::ffff:10.0.0.0/104").unwrap();
        assert!(
            mapped_cluster.contains(v4addr("10.1.2.3")),
            "::ffff:10.0.0.0/104 canonicalizes to 10.0.0.0/8"
        );
        assert_eq!(
            classify_egress_ip(v4addr("10.1.2.3"), &[mapped_cluster]).unwrap_err(),
            EgressBlock::Configured
        );
        // A v6 CIDR wide enough to cover the mapped block catches the mapped
        // destination through the ORIGINAL (v6) form — the `extra_blocked`
        // loop tests both the original and the canonicalized destination.
        let all_v6 = EgressCidr::parse("::/0").unwrap();
        assert_eq!(
            classify_egress_ip(v6addr("::ffff:93.184.216.34"), &[all_v6]).unwrap_err(),
            EgressBlock::Configured,
            "::/0 covers the mapped block in v6 form"
        );
        // A native mapped CIDR (`::ffff:0:0/96`) canonicalizes to
        // `0.0.0.0/0` at parse and catches the mapped form as v4.
        let mapped_block = EgressCidr::parse("::ffff:0:0/96").unwrap();
        assert_eq!(
            classify_egress_ip(v6addr("::ffff:8.8.8.8"), &[mapped_block]).unwrap_err(),
            EgressBlock::Configured
        );
        // Control: a v6 CIDR that does not cover the mapped block does not
        // catch the mapped destination (the double test does not widen it).
        let doc_range = EgressCidr::parse("2001:db8::/32").unwrap();
        assert!(
            classify_egress_ip(v6addr("::ffff:8.8.8.8"), &[doc_range]).is_ok(),
            "2001:db8::/32 neither contains the v6 original nor the v4 canonical form"
        );
        // A configured CIDR can also override the IANA reason of an address
        // it covers (operator intent wins, most-specific-by-configuration).
        assert_eq!(
            classify_egress_ip(v4addr("10.42.0.5"), &[node]).unwrap_err(),
            EgressBlock::Private,
            "10.42.0.5 is outside the node CIDR and keeps its IANA reason"
        );
    }

    /// `EgressBlock` renders stable snake_case for the event and the log.
    #[test]
    fn egress_block_displays_snake_case() {
        let cases = [
            (EgressBlock::Loopback, "loopback"),
            (EgressBlock::Private, "private"),
            (EgressBlock::LinkLocal, "link_local"),
            (EgressBlock::SharedCgnat, "shared_cgnat"),
            (EgressBlock::Unspecified, "unspecified"),
            (EgressBlock::Multicast, "multicast"),
            (EgressBlock::Broadcast, "broadcast"),
            (EgressBlock::Documentation, "documentation"),
            (EgressBlock::Benchmarking, "benchmarking"),
            (EgressBlock::Reserved, "reserved"),
            (EgressBlock::IetfProtocol, "ietf_protocol"),
            (EgressBlock::Tunnel6to4, "tunnel_6to4"),
            (EgressBlock::Teredo, "teredo"),
            (EgressBlock::NotGlobalUnicastV6, "not_global_unicast_v6"),
            (EgressBlock::ThisNetwork, "this_network"),
            (EgressBlock::Nat64, "nat64"),
            (EgressBlock::Configured, "configured"),
        ];
        for (reason, text) in cases {
            assert_eq!(reason.to_string(), text);
        }
    }
}
