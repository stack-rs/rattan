use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt::Display;
use std::net::IpAddr;
use std::process::Command;

pub fn add_runtime_env_var(
    handle: &mut Command,
    ip_list: Vec<(usize, IpAddr)>,
    rattan_id: impl Into<String>,
) {
    for (i, ip) in ip_list.into_iter() {
        handle.env(format!("RATTAN_IP_{i}"), ip.to_string());
        match i {
            0 => {
                handle.env("RATTAN_EXT", ip.to_string());
            }
            1 => {
                handle.env("RATTAN_BASE", ip.to_string());
            }
            _ => {}
        };
    }
    handle.env("RATTAN_ID", rattan_id.into());
}

#[derive(Clone)]
pub struct CowStr<'a>(Cow<'a, str>);

impl<'a> Display for CowStr<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Cow::Borrowed(a) => write!(f, "{}", a),
            Cow::Owned(a) => write!(f, "{}", a),
        }
    }
}

impl AsRef<OsStr> for CowStr<'_> {
    fn as_ref(&self) -> &OsStr {
        self.0.as_ref().as_ref()
    }
}

impl<'a> From<Cow<'a, str>> for CowStr<'a> {
    fn from(value: Cow<'a, str>) -> Self {
        Self(value)
    }
}

impl From<String> for CowStr<'static> {
    fn from(val: String) -> Self {
        CowStr(Cow::Owned(val))
    }
}

impl<'a> From<&'a str> for CowStr<'a> {
    fn from(val: &'a str) -> Self {
        CowStr(Cow::Borrowed(val))
    }
}

/// The caller should make sure that there are at least 1 item in the `cmd`.
pub fn build_command<'a>(mut cmd: impl Iterator<Item = CowStr<'a>>) -> Command {
    let program = cmd.next().unwrap();
    let mut command = Command::new(program);
    command.args(cmd);
    command
}
