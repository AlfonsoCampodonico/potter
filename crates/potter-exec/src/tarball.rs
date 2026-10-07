//! Deterministic tar layers: normalised metadata, GNU long names, no user or group names.

use std::io::{self, Read, Write};

use tar::{Builder, EntryType, Header};

/// An entry's metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub mode: u32,
    pub uid: u64,
    pub gid: u64,
    pub mtime: u64,
}

impl Meta {
    /// Root-owned with `mode`, at `mtime`.
    pub fn root(mode: u32, mtime: u64) -> Self {
        Self {
            mode,
            uid: 0,
            gid: 0,
            mtime,
        }
    }
}

fn header(kind: EntryType, m: &Meta, size: u64) -> Header {
    let mut h = Header::new_gnu();
    h.set_entry_type(kind);
    h.set_mode(m.mode);
    h.set_uid(m.uid);
    h.set_gid(m.gid);
    h.set_mtime(m.mtime);
    h.set_size(size);
    h
}

/// Reads exactly `remaining` bytes, failing if the source ends first (a file that shrank
/// while it was archived would otherwise leave a corrupt entry).
struct Exact<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for Exact<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let want = buf.len().min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the file shrank while it was read",
            ));
        }
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// `name`'s whiteout path: `.wh.<name>` in the same directory.
fn whiteout_path(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, name)) => format!("{dir}/.wh.{name}"),
        None => format!(".wh.{path}"),
    }
}

/// Writes tar entries exactly as given: the caller orders them.
pub struct TarWriter<W: Write> {
    b: Builder<W>,
}

impl<W: Write> TarWriter<W> {
    pub fn new(out: W) -> Self {
        let mut b = Builder::new(out);
        b.follow_symlinks(false);
        Self { b }
    }

    pub fn dir(&mut self, path: &str, m: &Meta) -> io::Result<()> {
        self.b
            .append_data(&mut header(EntryType::Directory, m, 0), path, io::empty())
    }

    /// A regular file of exactly `size` bytes read from `data`.
    pub fn file(&mut self, path: &str, m: &Meta, size: u64, data: &mut dyn Read) -> io::Result<()> {
        let exact = Exact {
            inner: data,
            remaining: size,
        };
        self.b
            .append_data(&mut header(EntryType::Regular, m, size), path, exact)
    }

    /// A regular file with extended attributes (PAX `SCHILY.xattr.<name>`).
    pub fn xattr_file(
        &mut self,
        path: &str,
        m: &Meta,
        xattrs: &[(String, Vec<u8>)],
        size: u64,
        data: &mut dyn Read,
    ) -> io::Result<()> {
        self.pax_xattrs(xattrs)?;
        self.file(path, m, size, data)
    }

    /// PAX `SCHILY.xattr.<name>` records for the next entry.
    pub fn pax_xattrs(&mut self, xattrs: &[(String, Vec<u8>)]) -> io::Result<()> {
        if xattrs.is_empty() {
            return Ok(());
        }
        let records: Vec<(String, &[u8])> = xattrs
            .iter()
            .map(|(k, v)| (format!("SCHILY.xattr.{k}"), v.as_slice()))
            .collect();
        self.b
            .append_pax_extensions(records.iter().map(|(k, v)| (k.as_str(), *v)))
    }

    pub fn symlink(&mut self, path: &str, target: &str, m: &Meta) -> io::Result<()> {
        self.b.append_link(&mut header(EntryType::Symlink, m, 0), path, target)
    }

    /// A hardlink to `target`, written earlier in the stream. `m` is the shared inode's
    /// metadata: readers (kiln, containerd) apply a link header's attributes to the inode.
    pub fn hardlink(&mut self, path: &str, target: &str, m: &Meta) -> io::Result<()> {
        self.b.append_link(&mut header(EntryType::Link, m, 0), path, target)
    }

    pub fn fifo(&mut self, path: &str, m: &Meta) -> io::Result<()> {
        self.b
            .append_data(&mut header(EntryType::Fifo, m, 0), path, io::empty())
    }

    /// Deletes `path` from lower layers: an empty `.wh.<name>` file.
    pub fn whiteout(&mut self, path: &str) -> io::Result<()> {
        self.b.append_data(
            &mut header(EntryType::Regular, &Meta::root(0, 0), 0),
            whiteout_path(path),
            io::empty(),
        )
    }

    /// Hides `dir`'s lower-layer contents: `<dir>/.wh..wh..opq`.
    pub fn opaque(&mut self, dir: &str) -> io::Result<()> {
        self.b.append_data(
            &mut header(EntryType::Regular, &Meta::root(0, 0), 0),
            format!("{dir}/.wh..wh..opq"),
            io::empty(),
        )
    }

    pub fn finish(self) -> io::Result<W> {
        self.b.into_inner()
    }
}

/// The `uid` and `gid` of `name` in passwd(5) text.
fn passwd_ids(passwd: &str, name: &str) -> Option<(u64, u64)> {
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 4 && f[0] == name).then(|| Some((f[2].parse().ok()?, f[3].parse().ok()?)))?
    })
}

/// The gid of `name` in group(5) text.
fn group_id(group: &str, name: &str) -> Option<u64> {
    group.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 3 && f[0] == name).then(|| f[2].parse().ok())?
    })
}

/// `user[:group]` as numeric ids: numbers, `root`, or names looked up in the image's
/// `/etc/passwd` and `/etc/group` text.
pub fn owner(spec: &str, passwd: Option<&str>, group: Option<&str>) -> Result<(u64, u64), String> {
    let (user, grp) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };
    if user.is_empty() || grp == Some("") {
        return Err(format!("the owner {spec:?} names no user or group"));
    }
    let (uid, primary) = match user.parse::<u64>() {
        Ok(n) => (n, n),
        Err(_) => match passwd {
            Some(p) => passwd_ids(p, user).ok_or_else(|| format!("no user {user:?} in the image's /etc/passwd"))?,
            None if user == "root" => (0, 0),
            None => return Err(format!("the image has no /etc/passwd to look up the user {user:?}")),
        },
    };
    let gid = match grp {
        None => primary,
        Some(g) => match g.parse::<u64>() {
            Ok(n) => n,
            Err(_) => match group {
                Some(t) => group_id(t, g).ok_or_else(|| format!("no group {g:?} in the image's /etc/group"))?,
                None if g == "root" => 0,
                None => return Err(format!("the image has no /etc/group to look up the group {g:?}")),
            },
        },
    };
    Ok((uid, gid))
}

/// `SOURCE_DATE_EPOCH`, or 0: every layer potter writes has this mtime.
pub fn epoch() -> u64 {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owners_parse_and_look_up() {
        let passwd = "root:x:0:0::/root:/bin/sh\nwww-data:x:33:33::/var/www:/usr/sbin/nologin\n";
        let group = "root:x:0:\nwww-data:x:33:\nstaff:x:50:\n";
        assert_eq!(owner("0:0", None, None), Ok((0, 0)));
        assert_eq!(owner("1000", None, None), Ok((1000, 1000)));
        assert_eq!(owner("1000:50", None, None), Ok((1000, 50)));
        assert_eq!(owner("root", None, None), Ok((0, 0)));
        assert_eq!(owner("www-data", Some(passwd), None), Ok((33, 33)));
        assert_eq!(owner("www-data:staff", Some(passwd), Some(group)), Ok((33, 50)));
        assert!(owner("nobody-here", Some(passwd), None).is_err());
        assert!(owner("www-data", None, None).is_err());
        assert!(owner("", None, None).is_err());
    }
}
