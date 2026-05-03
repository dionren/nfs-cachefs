//! Parse the state line returned by `read(/dev/cachefiles)`.
//!
//! Format (from `fs/cachefiles/daemon.c::cachefiles_daemon_read`):
//!
//! ```text
//! cull=N frun=H fcull=H fstop=H brun=H bcull=H bstop=H
//! ```
//!
//! - `cull` is `1` if the cache is below the bcull/fcull threshold and the
//!   daemon should start culling, `0` otherwise.
//! - The other fields are current free counts, hex-formatted (no `0x`
//!   prefix). `f*` are inode counts; `b*` are block counts.
//!
//! There is no trailing newline. Whitespace separates fields.

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheState {
    pub culling: bool,
    pub frun: u64,
    pub fcull: u64,
    pub fstop: u64,
    pub brun: u64,
    pub bcull: u64,
    pub bstop: u64,
}

impl CacheState {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        let s = std::str::from_utf8(buf)
            .map_err(|e| Error::protocol(format!("non-UTF-8 state line: {e}")))?;
        let s = s.trim();
        let mut culling = None;
        let mut frun = None;
        let mut fcull = None;
        let mut fstop = None;
        let mut brun = None;
        let mut bcull = None;
        let mut bstop = None;

        for tok in s.split_ascii_whitespace() {
            let (k, v) = tok
                .split_once('=')
                .ok_or_else(|| Error::protocol(format!("malformed token {tok:?} in {s:?}")))?;
            match k {
                "cull" => {
                    culling = Some(match v {
                        "0" => false,
                        "1" => true,
                        other => {
                            return Err(Error::protocol(format!(
                                "expected cull=0|1, got cull={other:?}"
                            )))
                        }
                    });
                }
                "frun" | "fcull" | "fstop" | "brun" | "bcull" | "bstop" => {
                    let n = u64::from_str_radix(v, 16).map_err(|e| {
                        Error::protocol(format!("hex parse {k}={v:?}: {e}"))
                    })?;
                    match k {
                        "frun" => frun = Some(n),
                        "fcull" => fcull = Some(n),
                        "fstop" => fstop = Some(n),
                        "brun" => brun = Some(n),
                        "bcull" => bcull = Some(n),
                        "bstop" => bstop = Some(n),
                        _ => unreachable!(),
                    }
                }
                _ => {} // ignore unknown fields for forward-compat
            }
        }

        Ok(CacheState {
            culling: culling
                .ok_or_else(|| Error::protocol(format!("no cull= in state line {s:?}")))?,
            frun: frun.ok_or_else(|| Error::protocol("no frun=".to_string()))?,
            fcull: fcull.ok_or_else(|| Error::protocol("no fcull=".to_string()))?,
            fstop: fstop.ok_or_else(|| Error::protocol("no fstop=".to_string()))?,
            brun: brun.ok_or_else(|| Error::protocol("no brun=".to_string()))?,
            bcull: bcull.ok_or_else(|| Error::protocol("no bcull=".to_string()))?,
            bstop: bstop.ok_or_else(|| Error::protocol("no bstop=".to_string()))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_typical_line() {
        let line = b"cull=0 frun=100000 fcull=ea60 fstop=36ee80 brun=ea60 bcull=ea60 bstop=1";
        let s = CacheState::parse(line).expect("parse");
        assert!(!s.culling);
        assert_eq!(s.frun, 0x100000);
        assert_eq!(s.fcull, 0xea60);
        assert_eq!(s.fstop, 0x36ee80);
        assert_eq!(s.bstop, 1);
    }

    #[test]
    fn culling_flag_is_one() {
        let line = b"cull=1 frun=0 fcull=0 fstop=0 brun=0 bcull=0 bstop=0";
        assert!(CacheState::parse(line).unwrap().culling);
    }

    #[test]
    fn rejects_missing_field() {
        let line = b"cull=0 frun=10 fcull=5 fstop=1 brun=10 bcull=5";
        assert!(CacheState::parse(line).is_err());
    }

    #[test]
    fn rejects_non_hex() {
        let line = b"cull=0 frun=zzz fcull=0 fstop=0 brun=0 bcull=0 bstop=0";
        assert!(CacheState::parse(line).is_err());
    }

    #[test]
    fn rejects_invalid_cull() {
        let line = b"cull=2 frun=0 fcull=0 fstop=0 brun=0 bcull=0 bstop=0";
        assert!(CacheState::parse(line).is_err());
    }

    #[test]
    fn ignores_unknown_field() {
        let line = b"cull=0 frun=10 fcull=5 fstop=1 brun=10 bcull=5 bstop=1 future=42";
        assert!(CacheState::parse(line).is_ok());
    }
}
