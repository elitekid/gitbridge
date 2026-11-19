//! 지역 시각과 임의 값. 외부 시간 크레이트 없이 Windows GetLocalTime 을 쓴다.

#[derive(Clone, Copy, Debug)]
pub struct Local {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub milli: u32,
}

#[cfg(windows)]
pub fn now() -> Local {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut st = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    Local {
        year: st.wYear as i32,
        month: st.wMonth as u32,
        day: st.wDay as u32,
        hour: st.wHour as u32,
        minute: st.wMinute as u32,
        second: st.wSecond as u32,
        milli: st.wMilliseconds as u32,
    }
}

#[cfg(not(windows))]
pub fn now() -> Local {
    // Windows 밖에서는 UTC 로 둔다(시험 빌드용).
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (y, m, dd) = civil_from_days(secs.div_euclid(86400));
    let rem = secs.rem_euclid(86400);
    Local {
        year: y,
        month: m,
        day: dd,
        hour: (rem / 3600) as u32,
        minute: (rem % 3600 / 60) as u32,
        second: (rem % 60) as u32,
        milli: d.subsec_millis(),
    }
}

impl Local {
    /// %Y%m%d%H%M%S
    pub fn stamp14(&self) -> String {
        format!("{:04}{:02}{:02}{:02}{:02}{:02}", self.year, self.month, self.day, self.hour, self.minute, self.second)
    }
    /// %Y%m%d%H%M%S%f (마이크로초 자리는 밀리초×1000)
    pub fn stamp20(&self) -> String {
        format!("{}{:06}", self.stamp14(), self.milli * 1000)
    }
    /// %Y%m%d-%H%M
    pub fn file_stem(&self) -> String {
        format!("{:04}{:02}{:02}-{:02}{:02}", self.year, self.month, self.day, self.hour, self.minute)
    }
    /// %Y-%m-%d %H:%M:%S
    pub fn display(&self) -> String {
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", self.year, self.month, self.day, self.hour, self.minute, self.second)
    }
    /// %Y-%m-%d
    pub fn date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
    /// %H:%M:%S
    pub fn hms(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }
    /// 지역 시각을 그대로 초로 센 값(비교용).
    pub fn seconds(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * 86400
            + (self.hour * 3600 + self.minute * 60 + self.second) as i64
    }
}

/// "%Y%m%d%H%M%S" 14자를 seconds() 와 같은 기준의 초로 바꾼다.
pub fn parse_stamp14(s: &str) -> Option<i64> {
    if s.len() < 14 || !s.as_bytes()[..14].iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |a: usize, b: usize| s[a..b].parse::<u32>().ok();
    let (y, mo, d, h, mi, se) = (n(0, 4)?, n(4, 6)?, n(6, 8)?, n(8, 10)?, n(10, 12)?, n(12, 14)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    Some(days_from_civil(y as i32, mo, d) * 86400 + (h * 3600 + mi * 60 + se) as i64)
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(not(windows))]
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

/// 임의의 16진 문자열(bytes×2 자).
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if getrandom::fill(&mut buf).is_err() {
        // 운영체제 난수를 못 얻으면 시각과 프로세스 번호로 대신한다(임시 이름 충돌 방지용일 뿐이다).
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let seed = t.as_nanos() as u64 ^ ((std::process::id() as u64) << 32);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (seed.rotate_left((i * 8) as u32 % 64) & 0xff) as u8;
        }
    }
    buf.iter().map(|b| format!("{:02x}", b)).collect()
}
