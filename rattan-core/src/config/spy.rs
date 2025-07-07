use std::{
    fs::{File, OpenOptions},
    ops::Deref as _,
};

use dyn_clone::DynClone;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{
    cells::{spy, Packet},
    core::CellFactory,
};

pub trait Write: std::io::Write + dyn_clone::DynClone {}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum SpyCellBuildConfig {
    Path(std::path::PathBuf),
    #[cfg_attr(feature = "serde", serde(skip))]
    Config(Box<dyn Write + Send>),
}

impl SpyCellBuildConfig {
    pub fn into_factory<P: Packet>(
        self,
    ) -> impl CellFactory<spy::SpyCell<P, Box<dyn std::io::Write + Send>>> {
        move |handle| {
            let _guard = handle.enter();
            match self {
                Self::Path(path) => {
                    let file = if path.exists() {
                        OpenOptions::new().write(true).truncate(true).open(&path)?
                    } else {
                        File::create_new(&path)?
                    };
                    let config: Box<dyn std::io::Write + Send> = Box::new(file);
                    // let config = spy::SpyCellConfig::<Box<dyn std::io::Write>>::new(config);
                    spy::SpyCell::new(config)
                }
                Self::Config(config) => spy::SpyCell::new(config),
            }
        }
    }
}

impl Clone for SpyCellBuildConfig {
    fn clone(&self) -> Self {
        match self {
            Self::Path(path) => Self::Path(path.clone()),
            Self::Config(config) => Self::Config(dyn_clone::clone_box(config.deref())),
        }
    }
}

impl std::fmt::Debug for SpyCellBuildConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(path) => write!(f, "Path({path:?})"),
            Self::Config(_config) => write!(f, "Config"),
        }
    }
}

impl From<Box<dyn Write + Send>> for Box<dyn std::io::Write + Send> {
    fn from(value: Box<dyn Write + Send>) -> Self {
        value
    }
}

impl<W: std::io::Write + DynClone> Write for W {}
