use std::ffi::CString;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;

const MAX_GROUPS: usize = 64;

#[cfg(target_os = "macos")]
type GroupId = libc::c_int;
#[cfg(not(target_os = "macos"))]
type GroupId = libc::gid_t;

/// The account behind `sudo`, so a root walk can tell which folders that
/// account could not have opened itself.
pub struct Invoker {
    uid: u32,
    gids: Vec<u32>,
}

impl Invoker {
    pub fn under_sudo() -> Option<Self> {
        // SAFETY: geteuid cannot fail and reads no memory we pass in.
        if unsafe { libc::geteuid() } != 0 {
            return None;
        }
        let user = std::env::var("SUDO_USER").ok().filter(|u| u != "root")?;
        let uid = std::env::var("SUDO_UID").ok()?.parse().ok()?;
        let gid: u32 = std::env::var("SUDO_GID").ok()?.parse().ok()?;
        Some(Self {
            uid,
            gids: groups_of(&user, gid),
        })
    }

    pub fn can_open(&self, meta: &Metadata) -> bool {
        let need = if meta.uid() == self.uid {
            0o500
        } else if self.gids.contains(&meta.gid()) {
            0o050
        } else {
            0o005
        };
        meta.mode() & need == need
    }
}

fn groups_of(user: &str, gid: u32) -> Vec<u32> {
    let Ok(name) = CString::new(user) else {
        return vec![gid];
    };
    let mut groups: Vec<GroupId> = vec![0; MAX_GROUPS];
    let mut count = MAX_GROUPS as libc::c_int;
    // SAFETY: `groups` holds `count` entries and getgrouplist writes at most that many.
    let status = unsafe {
        libc::getgrouplist(
            name.as_ptr(),
            gid as GroupId,
            groups.as_mut_ptr(),
            &mut count,
        )
    };
    if status < 0 {
        return vec![gid];
    }
    groups.truncate(count.max(0) as usize);
    groups.into_iter().map(|g| g as u32).collect()
}
