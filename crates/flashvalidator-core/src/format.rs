//! Human-readable formatting helpers.

/// Formats a byte count with decimal units, as drive vendors do, e.g. `31.9 GB`.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

/// Formats a byte count with binary units, e.g. `4 KiB`, `8 MiB`, `16 GiB`.
/// Used for transfer sizes and wrap periods, which are powers of two.
pub fn human_bytes_binary(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value.fract() == 0.0 { format!("{value:.0} {}", UNITS[unit]) } else { format!("{value:.1} {}", UNITS[unit]) }
}

/// Formats an integer with thousands separators, e.g. `31,914,983,424`.
pub fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `31.9 GB (31,914,983,424 bytes)`
pub fn bytes_detailed(bytes: u64) -> String {
    format!("{} ({} bytes)", human_bytes(bytes), grouped(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(31_914_983_424), "31.9 GB");
        assert_eq!(human_bytes_binary(4096), "4 KiB");
        assert_eq!(human_bytes_binary(16 << 30), "16 GiB");
        assert_eq!(human_bytes_binary(1536), "1.5 KiB");
        assert_eq!(grouped(1234567), "1,234,567");
        assert_eq!(grouped(12), "12");
    }
}
