use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::{AsRawFd, IntoRawFd};
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};

use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{setns, unshare, CloneFlags};
use nix::unistd::gettid;

use crate::error::NsError;

/// Defines a NetNs environment behavior.
pub trait Env {
    /// The persist directory of the NetNs environment.
    fn persist_dir(&self) -> PathBuf;

    /// Returns `true` if the given path is in this Env.
    fn contains<P: AsRef<Path>>(&self, p: P) -> bool {
        p.as_ref().starts_with(self.persist_dir())
    }

    /// Initialize the environment.
    fn init(&self) -> Result<(), NsError> {
        // Create the directory for mounting network namespaces.
        // This needs to be a shared mount-point in case it is mounted in to
        // other namespaces (containers)
        let persist_dir = self.persist_dir();
        std::fs::create_dir_all(&persist_dir).map_err(NsError::CreateNsDirError)?;

        // Remount the namespace directory shared. This will fail if it is not
        // already a mount-point, so bind-mount it on to itself to "upgrade" it
        // to a mount-point.
        let mut made_netns_persist_dir_mount: bool = false;
        while let Err(e) = mount(
            Some(""),
            &persist_dir,
            Some("none"),
            MsFlags::MS_SHARED | MsFlags::MS_REC,
            Some(""),
        ) {
            // Fail unless we need to make the mount point
            if e != nix::errno::Errno::EINVAL || made_netns_persist_dir_mount {
                return Err(NsError::MountError(
                    format!("(SHARED|REC) {}", persist_dir.display()),
                    e,
                ));
            }
            // Recursively remount /var/<persist> on itself. The recursive flag is
            // so that any existing netns bind-mounts are carried over.
            mount(
                Some(&persist_dir),
                &persist_dir,
                Some("none"),
                MsFlags::MS_BIND | MsFlags::MS_REC,
                Some(""),
            )
            .map_err(|e| {
                NsError::MountError(
                    format!(
                        "(BIND|REC) {} to {}",
                        persist_dir.display(),
                        persist_dir.display()
                    ),
                    e,
                )
            })?;
            made_netns_persist_dir_mount = true;
        }
        Ok(())
    }
}

/// A default network namespace environment.
///
/// Its persistence directory is `/var/run/netns`, which is for consistency with the `ip-netns` tool.
/// See [ip-netns](https://man7.org/linux/man-pages/man8/ip-netns.8.html) for details.
#[derive(Copy, Clone, Default, Debug)]
pub struct DefaultEnv;

impl Env for DefaultEnv {
    fn persist_dir(&self) -> PathBuf {
        PathBuf::from("/var/run/netns")
    }
}

/// A network namespace type.
///
/// It could be used to enter network namespace.
#[derive(Debug)]
pub struct NetNs<E: Env = DefaultEnv> {
    file: File,
    path: PathBuf,
    env: Option<E>,
}

impl<E: Env> AsRawFd for NetNs<E> {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.file.as_raw_fd()
    }
}

impl<E: Env> std::fmt::Display for NetNs<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if let Ok(meta) = self.file.metadata() {
            write!(
                f,
                "NetNS {{ fd: {}, dev: {}, ino: {}, path: {} }}",
                self.as_raw_fd(),
                meta.dev(),
                meta.ino(),
                self.path.display()
            )
        } else {
            write!(
                f,
                "NetNS {{ fd: {}, path: {} }}",
                self.as_raw_fd(),
                self.path.display()
            )
        }
    }
}

impl<E1: Env, E2: Env> PartialEq<NetNs<E1>> for NetNs<E2> {
    fn eq(&self, other: &NetNs<E1>) -> bool {
        if self.as_raw_fd() == other.as_raw_fd() {
            return true;
        }
        let cmp_meta = |f1: &File, f2: &File| -> Option<bool> {
            let m1 = match f1.metadata() {
                Ok(m) => m,
                Err(_) => return None,
            };
            let m2 = match f2.metadata() {
                Ok(m) => m,
                Err(_) => return None,
            };
            Some(m1.dev() == m2.dev() && m1.ino() == m2.ino())
        };
        cmp_meta(&self.file, &other.file).unwrap_or_else(|| self.path == other.path)
    }
}

impl<E: Env> NetNs<E> {
    /// Creates a new `NetNs` with the specified name and Env.
    ///
    /// The persist dir of network namespace will be created if it doesn't already exist.
    pub fn new_with_env<S: AsRef<str>>(ns_name: S, env: E) -> Result<Self, NsError> {
        env.init()?;

        // create an empty file at the mount point
        let ns_path = env.persist_dir().join(ns_name.as_ref());
        let _ = File::create(&ns_path).map_err(NsError::CreateNsError)?;
        Self::persistent(&ns_path, true).map_err(|e| {
            // Ensure the mount point is cleaned up on errors; if the namespace was successfully
            // mounted this will have no effect because the file is in-use
            std::fs::remove_file(&ns_path).ok();
            e
        })?;
        Self::get_from_env(ns_name, env)
    }

    fn persistent<P: AsRef<Path>>(ns_path: &P, new_thread: bool) -> Result<(), NsError> {
        if new_thread {
            let ns_path_clone = ns_path.as_ref().to_path_buf();
            let new_thread: JoinHandle<Result<(), NsError>> =
                thread::spawn(move || Self::persistent(&ns_path_clone, false));
            match new_thread.join() {
                Ok(t) => t?,
                Err(e) => {
                    return Err(NsError::JoinThreadError(format!("{:?}", e)));
                }
            };
        } else {
            // Create a new netns for the current thread.
            unshare(CloneFlags::CLONE_NEWNET).map_err(NsError::UnshareError)?;
            // bind mount the netns from the current thread (from /proc) onto the mount point.
            // This persists the ns, even when there are no threads in the ns.
            let src = get_current_netns_path();
            mount(
                Some(src.as_path()),
                ns_path.as_ref(),
                Some("none"),
                MsFlags::MS_BIND,
                Some(""),
            )
            .map_err(|e| {
                NsError::MountError(
                    format!("(BIND) {} to {}", src.display(), ns_path.as_ref().display()),
                    e,
                )
            })?;
        }
        Ok(())
    }

    /// Gets the path of this NetNs.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Gets the Env of this NetNs.
    pub fn env(&self) -> Option<&E> {
        self.env.as_ref()
    }

    /// Gets the Env of this network namespace.
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Makes the current thread enter this network namespace.
    ///
    /// Requires elevated privileges.
    pub fn enter(&self) -> Result<(), NsError> {
        setns(self.as_raw_fd(), CloneFlags::CLONE_NEWNET).map_err(NsError::SetNsError)
    }

    /// Returns the NetNs with the specified name and Env.
    pub fn get_from_env<S: AsRef<str>>(ns_name: S, env: E) -> Result<Self, NsError> {
        let ns_path = env.persist_dir().join(ns_name.as_ref());
        let file = File::open(&ns_path).map_err(|e| NsError::OpenNsError(ns_path.clone(), e))?;

        Ok(Self {
            file,
            path: ns_path,
            env: Some(env),
        })
    }

    /// Removes this network namespace manually.
    ///
    /// Once called, this instance will not be available.
    pub fn remove(self) -> Result<(), NsError> {
        // need close first
        nix::unistd::close(self.file.into_raw_fd()).map_err(NsError::CloseNsError)?;
        // Only unmount if it's been bind-mounted (don't touch namespaces in /proc...)
        if let Some(env) = &self.env {
            if env.contains(&self.path) {
                Self::umount_ns(&self.path)?;
            }
        }
        Ok(())
    }

    fn umount_ns<P: AsRef<Path>>(path: P) -> Result<(), NsError> {
        let path = path.as_ref();
        umount2(path, MntFlags::MNT_DETACH)
            .map_err(|e| NsError::UnmountError(path.to_owned(), e))?;
        let _ = std::fs::remove_file(path);
        Ok(())
    }
}

impl NetNs {
    /// Creates a new persistent (bind-mounted) network namespace and returns an object representing
    /// that namespace, without switching to it.
    ///
    /// The persist directory of network namespace will be created if it doesn't already exist.
    /// This function will use [`DefaultEnv`] to create persist directory.
    ///
    /// Requires elevated privileges.
    ///
    /// [`DefaultEnv`]: DefaultEnv
    ///
    pub fn new<S: AsRef<str>>(ns_name: S) -> Result<Self, NsError> {
        Self::new_with_env(ns_name, DefaultEnv)
    }

    /// Returns the NetNs with the specified name and `DefaultEnv`.
    pub fn get<S: AsRef<str>>(ns_name: S) -> Result<Self, NsError> {
        Self::get_from_env(ns_name, DefaultEnv)
    }
}

/// Returns the NetNs with the specified path.
pub fn get_netns_from_path<P: AsRef<Path>>(ns_path: P) -> Result<NetNs, NsError> {
    let ns_path = ns_path.as_ref().to_path_buf();
    let file = File::open(&ns_path).map_err(|e| NsError::OpenNsError(ns_path.clone(), e))?;

    Ok(NetNs {
        file,
        path: ns_path,
        env: None,
    })
}

/// Returns the NetNs of current thread.
pub fn get_current_netns() -> Result<NetNs, NsError> {
    let ns_path = get_current_netns_path();
    let file = File::open(&ns_path).map_err(|e| NsError::OpenNsError(ns_path.clone(), e))?;

    Ok(NetNs {
        file,
        path: ns_path,
        env: None,
    })
}

#[inline]
fn get_current_netns_path() -> PathBuf {
    PathBuf::from(format!("/proc/self/task/{}/ns/net", gettid()))
}
