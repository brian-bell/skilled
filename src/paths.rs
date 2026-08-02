use std::{ffi::OsString, path::PathBuf};

use directories::{ProjectDirs, UserDirs};

use crate::{Error, Result};

#[derive(Clone, Debug)]
pub struct AppEnvironment {
    pub home_dir: PathBuf,
    pub data_dir: PathBuf,
    pub executable_path: OsString,
}

impl AppEnvironment {
    pub fn new(
        home_dir: impl Into<PathBuf>,
        data_dir: impl Into<PathBuf>,
        executable_path: impl Into<OsString>,
    ) -> Self {
        Self {
            home_dir: home_dir.into(),
            data_dir: data_dir.into(),
            executable_path: executable_path.into(),
        }
    }

    pub fn for_process() -> Result<Self> {
        let home_dir = UserDirs::new()
            .map(|directories| directories.home_dir().to_owned())
            .ok_or(Error::HomeDirectoryUnavailable)?;
        let data_dir = ProjectDirs::from("", "", "skilled")
            .map(|directories| directories.data_dir().to_owned())
            .ok_or(Error::DataDirectoryUnavailable)?;
        let executable_path = std::env::var_os("PATH").unwrap_or_default();
        Ok(Self::new(home_dir, data_dir, executable_path))
    }
}
