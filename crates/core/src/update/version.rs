//! Port of `packages/core/src/update/version.ts` — strict semver with the
//! same prerelease-comparison rules; unparseable input compares Equal so a
//! bad tag can never claim an update.
use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq)]
pub struct SemVer {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub prerelease: Vec<String>,
}

fn ident_charset_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

pub fn parse_version(input: &str) -> Option<SemVer> {
    let s = input.trim();
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
    let (s, build) = match s.split_once('+') {
        Some((head, b)) => (head, Some(b)),
        None => (s, None),
    };
    if let Some(b) = build {
        if !ident_charset_ok(b) {
            return None;
        }
    }
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (s, None),
    };
    let mut nums = core.split('.');
    let (a, b, c) = (nums.next()?, nums.next()?, nums.next()?);
    if nums.next().is_some() {
        return None;
    }
    let num = |t: &str| -> Option<u64> {
        (!t.is_empty() && t.chars().all(|ch| ch.is_ascii_digit())).then(|| t.parse().ok())?
    };
    let prerelease = match pre {
        Some(p) => {
            if !ident_charset_ok(p) {
                return None;
            }
            p.split('.').map(String::from).collect()
        }
        None => Vec::new(),
    };
    Some(SemVer {
        major: num(a)?,
        minor: num(b)?,
        patch: num(c)?,
        prerelease,
    })
}

/// TS `cmpPrerelease`: no-prerelease ranks above prerelease; numeric
/// identifiers rank below alphanumeric; element-wise, then by length.
fn cmp_prerelease(a: &[String], b: &[String]) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        _ => {}
    }
    for (ai, bi) in a.iter().zip(b.iter()) {
        let an = ai.chars().all(|c| c.is_ascii_digit()) && !ai.is_empty();
        let bn = bi.chars().all(|c| c.is_ascii_digit()) && !bi.is_empty();
        let c = match (an, bn) {
            (true, true) => ai
                .parse::<u64>()
                .unwrap_or(0)
                .cmp(&bi.parse::<u64>().unwrap_or(0)),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => ai.cmp(bi),
        };
        if c != Ordering::Equal {
            return c;
        }
    }
    a.len().cmp(&b.len())
}

pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let (Some(pa), Some(pb)) = (parse_version(a), parse_version(b)) else {
        return Ordering::Equal; // unparseable → never claim an update
    };
    pa.major
        .cmp(&pb.major)
        .then(pa.minor.cmp(&pb.minor))
        .then(pa.patch.cmp(&pb.patch))
        .then_with(|| cmp_prerelease(&pa.prerelease, &pb.prerelease))
}

pub fn is_newer(current: &str, latest: &str) -> bool {
    compare_versions(latest, current) == Ordering::Greater
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn parses_plain_and_v_prefixed_and_build_metadata() {
        let v = parse_version("v1.2.3").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 2, 3));
        assert!(v.prerelease.is_empty());
        let v = parse_version("1.2.3-rc.1+build.5").unwrap();
        assert_eq!(v.prerelease, vec!["rc", "1"]);
        assert!(parse_version("1.2").is_none());
        assert!(parse_version("x.y.z").is_none());
        assert!(parse_version("1.2.3-").is_none());
    }

    #[test]
    fn compare_orders_core_then_prerelease() {
        assert_eq!(compare_versions("1.2.3", "1.2.4"), Ordering::Less);
        assert_eq!(compare_versions("2.0.0", "1.9.9"), Ordering::Greater);
        // no-prerelease ranks ABOVE prerelease
        assert_eq!(compare_versions("1.0.0", "1.0.0-rc.1"), Ordering::Greater);
        // numeric identifiers rank below alphanumeric
        assert_eq!(compare_versions("1.0.0-1", "1.0.0-alpha"), Ordering::Less);
        assert_eq!(compare_versions("1.0.0-rc.1", "1.0.0-rc.2"), Ordering::Less);
        // shorter prerelease list ranks lower when prefixes equal
        assert_eq!(compare_versions("1.0.0-rc", "1.0.0-rc.1"), Ordering::Less);
        // unparseable → Equal (never claims an update)
        assert_eq!(compare_versions("junk", "1.0.0"), Ordering::Equal);
    }

    #[test]
    fn is_newer_matrix() {
        assert!(is_newer("0.2.0", "0.3.0"));
        assert!(!is_newer("0.3.0", "0.3.0"));
        assert!(!is_newer("0.3.0", "0.2.9"));
        assert!(!is_newer("0.3.0", "not-a-version"));
    }
}
