use std::time::{SystemTime, UNIX_EPOCH};
use winapi::um::minwinbase::SYSTEMTIME;
use winapi::um::timezoneapi::{SystemTimeToTzSpecificLocalTime, TzSpecificLocalTimeToSystemTime};

pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\n' | b'\r' => continue,
            _ => return None,
        } as u32;
        buf = buf << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// NUL-terminated UTF-16 for Win32 calls.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn rfc3339_utc(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

// Howard Hinnant's days-since-epoch to civil date algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Inverse of `civil_from_days`.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)` into unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |at: usize, n: usize| -> Option<i64> { s.get(at..at + n)?.parse::<i64>().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (h, mi, sec) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    let offset = match b.get(i) {
        None | Some(b'Z') | Some(b'z') => 0,
        Some(&c) if c == b'+' || c == b'-' => {
            let sign = if c == b'+' { 1 } else { -1 };
            let oh = num(i + 1, 2)?;
            let om = if b.get(i + 3) == Some(&b':') { num(i + 4, 2)? } else { num(i + 3, 2)? };
            sign * (oh * 3600 + om * 60)
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo as u32, d as u32) * 86400 + h * 3600 + mi * 60 + sec - offset)
}

/// Broken-down wall-clock time in the user's time zone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LocalTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Sunday.
    pub weekday: u32,
}

impl LocalTime {
    /// Days since the epoch of this calendar date; two times on the same local day share it.
    pub fn days(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    /// `HH:MM`, as shown in the time fields.
    pub fn hm(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }
}

fn systemtime_utc(secs: i64) -> SYSTEMTIME {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    SYSTEMTIME {
        wYear: y as u16,
        wMonth: m as u16,
        wDayOfWeek: (days + 4).rem_euclid(7) as u16,
        wDay: d as u16,
        wHour: (rem / 3600) as u16,
        wMinute: (rem % 3600 / 60) as u16,
        wSecond: (rem % 60) as u16,
        wMilliseconds: 0,
    }
}

/// Converts a unix timestamp to local wall-clock time (honours DST via the Windows tz database).
pub fn to_local(secs: i64) -> LocalTime {
    let utc = systemtime_utc(secs);
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut st) } == 0 {
        st = utc;
    }
    let (year, month, day) = (st.wYear as i64, st.wMonth as u32, st.wDay as u32);
    LocalTime {
        year,
        month,
        day,
        hour: st.wHour as u32,
        minute: st.wMinute as u32,
        second: st.wSecond as u32,
        weekday: (days_from_civil(year, month, day) + 4).rem_euclid(7) as u32,
    }
}

/// Inverse of `to_local`: a local wall-clock time to unix seconds.
pub fn from_local(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
    let local = SYSTEMTIME {
        wYear: year as u16,
        wMonth: month as u16,
        wDayOfWeek: 0,
        wDay: day as u16,
        wHour: hour as u16,
        wMinute: minute as u16,
        wSecond: second as u16,
        wMilliseconds: 0,
    };
    let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
    if unsafe { TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local, &mut utc) } == 0 {
        return days_from_civil(year, month, day) * 86400 + (hour * 3600 + minute * 60 + second) as i64;
    }
    days_from_civil(utc.wYear as i64, utc.wMonth as u32, utc.wDay as u32) * 86400
        + (utc.wHour as i64) * 3600
        + (utc.wMinute as i64) * 60
        + utc.wSecond as i64
}

/// Parses a time of day typed by the user: `9:30`, `09:30`, `9:30:00`, `930`, `1330` or `9`.
/// Full-width digits and colons (as the Japanese IME produces them) are accepted too.
pub fn parse_hm(text: &str) -> Option<(u32, u32)> {
    let s: String = text
        .trim()
        .chars()
        .map(|c| match c {
            '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32).unwrap_or(c),
            '：' => ':',
            _ => c,
        })
        .collect();
    let (h, m) = if let Some((h, rest)) = s.split_once(':') {
        let m = rest.split(':').next().unwrap_or("");
        if m.len() != 2 {
            return None;
        }
        (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?)
    } else {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        match s.len() {
            1 | 2 => (s.parse::<u32>().ok()?, 0),
            3 | 4 => {
                let (h, m) = s.split_at(s.len() - 2);
                (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?)
            }
            _ => return None,
        }
    };
    (h < 24 && m < 60).then_some((h, m))
}

pub fn fmt_hms(secs: i64) -> String {
    let s = secs.max(0);
    format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_roundtrip() {
        let cases: [&[u8]; 8] = [b"", b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar", &[0, 255, 7, 128]];
        for input in cases {
            assert_eq!(base64_decode(&base64(input)).as_deref(), Some(input));
        }
        assert_eq!(base64_decode("Zm9v\n"), Some(b"foo".to_vec()));
        assert_eq!(base64_decode("not base64!"), None);
    }

    #[test]
    fn rfc3339_known_dates() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(951782400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1758067199), "2025-09-16T23:59:59Z");
    }

    #[test]
    fn rfc3339_roundtrip() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2025-09-16T23:59:59Z"), Some(1758067199));
        assert_eq!(parse_rfc3339("2025-09-17T08:59:59+09:00"), Some(1758067199));
        assert_eq!(parse_rfc3339("2025-09-16T23:59:59.123456+00:00"), Some(1758067199));
        assert_eq!(parse_rfc3339("2000-02-29T00:00:00-05:00"), Some(951782400 + 5 * 3600));
        assert_eq!(parse_rfc3339("garbage"), None);
        for t in [0, 951782400, 1758067199, 1789000000] {
            assert_eq!(parse_rfc3339(&rfc3339_utc(t)), Some(t));
        }
    }

    #[test]
    fn hms() {
        assert_eq!(fmt_hms(0), "0:00:00");
        assert_eq!(fmt_hms(3661), "1:01:01");
        assert_eq!(fmt_hms(-5), "0:00:00");
    }

    #[test]
    fn local_roundtrip() {
        // Noon UTC is never inside a DST transition, whatever the machine's zone.
        for t in [0, 951782400 + 12 * 3600, 1758067200 + 12 * 3600, 1789000000] {
            let l = to_local(t);
            assert_eq!(from_local(l.year, l.month, l.day, l.hour, l.minute, l.second), t);
            assert!(l.weekday < 7);
        }
        // Same calendar day at 00:00 sorts before 23:59 and shares `days()`.
        let l = to_local(1758067200 + 12 * 3600);
        let a = to_local(from_local(l.year, l.month, l.day, 0, 0, 0));
        let b = to_local(from_local(l.year, l.month, l.day, 23, 59, 0));
        assert_eq!(a.days(), b.days());
        assert_eq!(b.hm(), "23:59");
    }

    #[test]
    fn parses_times_of_day() {
        assert_eq!(parse_hm("9:30"), Some((9, 30)));
        assert_eq!(parse_hm(" 09:30 "), Some((9, 30)));
        assert_eq!(parse_hm("23:59:59"), Some((23, 59)));
        assert_eq!(parse_hm("930"), Some((9, 30)));
        assert_eq!(parse_hm("1330"), Some((13, 30)));
        assert_eq!(parse_hm("9"), Some((9, 0)));
        assert_eq!(parse_hm("00"), Some((0, 0)));
        assert_eq!(parse_hm("０９：３０"), Some((9, 30)));
        assert_eq!(parse_hm("24:00"), None);
        assert_eq!(parse_hm("9:60"), None);
        assert_eq!(parse_hm("9:3"), None);
        assert_eq!(parse_hm(""), None);
        assert_eq!(parse_hm("abc"), None);
        assert_eq!(parse_hm("12345"), None);
    }
}
