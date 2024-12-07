use std::sync::mpsc::SendError;

use plist_plus::error::PlistError;
use rusty_libimobiledevice::error::{AfcError, IdeviceError, InstProxyError, LockdowndError};
use thiserror::Error;

use crate::device_syslog::LoggerCommand;

#[derive(Debug, Error)]
pub enum DeviceClientError {
    #[error("IDevice Error: {0}")]
    IDeviceError(#[from] IdeviceError),

    #[error("Lockdownd Error: {0}")]
    LockdowndError(#[from] LockdowndError),

    #[error("Device Not Found, Make Sure it's Plugged")]
    DeviceNotFound,

    #[error("AFC Client Error: {0}")]
    AFCClientError(#[from] AfcError),
}

#[derive(Debug, Error)]
pub enum DeviceSysLogError {
    #[error("Couldn't send a message to the channel, maybe it's closed?, error: {0}")]
    SendError(#[from] SendError<LoggerCommand>),
}

#[derive(Debug, Error)]
pub enum DeviceInstallerError {
    #[error("Couldn't create a folder")]
    ErrorCreatingFolder,

    #[error("IO Error: {0}")]
    IOError(#[from] std::io::Error),

    #[error("Zip Error: {0}")]
    ZipError(#[from] zip::result::ZipError),

    #[error("AFC Client Error: {0}")]
    AfcClientError(#[from] AfcError),

    #[error("The given package is neithr an ipa or an ipcc")]
    UnknownPackage,

    #[error("Plist Error: {0}")]
    PlistError(#[from] PlistError),

    #[error("Installation Proxy Error: {0}")]
    InstallationProxyError(#[from] InstProxyError),

    #[error("Device was not found, make sure it's connected")]
    DeviceNotFound,
}

#[derive(Debug, Error)]
pub enum IDeviceErrors {
    #[error("Lockdownd Error: {0}")]
    LockdowndError(#[from] LockdowndError),

    #[error("Plist Error: {0}")]
    PlistError(#[from] PlistError),

    #[error("Key not found")]
    KeyNotFound,
}
