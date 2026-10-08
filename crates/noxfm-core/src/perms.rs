/// `0o100755` -> `"rwxr-xr-x"`, with setuid/setgid/sticky shown ls-style.
pub fn symbolic(mode: u32) -> String {
    let bit = |m: u32, c: char| if mode & m != 0 { c } else { '-' };
    let special = |exec: u32, flag: u32, on: char, off: char| match (mode & exec != 0, mode & flag != 0) {
        (true, true) => on,
        (false, true) => off,
        (true, false) => 'x',
        (false, false) => '-',
    };
    [
        bit(0o400, 'r'),
        bit(0o200, 'w'),
        special(0o100, 0o4000, 's', 'S'),
        bit(0o040, 'r'),
        bit(0o020, 'w'),
        special(0o010, 0o2000, 's', 'S'),
        bit(0o004, 'r'),
        bit(0o002, 'w'),
        special(0o001, 0o1000, 't', 'T'),
    ]
    .into_iter()
    .collect()
}

/// `0o100755` -> `"0755"`
pub fn octal(mode: u32) -> String {
    format!("{:04o}", mode & 0o7777)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(symbolic(0o100755), "rwxr-xr-x");
        assert_eq!(symbolic(0o100644), "rw-r--r--");
        assert_eq!(symbolic(0o41777), "rwxrwxrwt");
        assert_eq!(symbolic(0o104755), "rwsr-xr-x");
        assert_eq!(symbolic(0o102644), "rw-r-Sr--");
        assert_eq!(octal(0o41777), "1777");
    }
}
