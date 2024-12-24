//! Provides an easy way to install iOS packages with different methods
//!
//! It allows users to install iOS packages to a device either from file paths or readers.
//! It supports installing `.ipa` (iOS application packages) and `.ipcc` (carrier bundle configuration files) packages.
//!
//! ## Features
//! - Installing from bytes
//! - Supporting ipa and ipcc packages
//!

use std::{
    collections::HashMap,
    ffi::OsStr,
    fmt::Display,
    io::{Cursor, Read, Seek, SeekFrom},
    marker::PhantomData,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use plist_plus::Plist;
use rusty_libimobiledevice::services::{
    afc::{AfcClient, AfcFileMode},
    instproxy::{CommandPlist, InstProxyClient, StatusPlist},
};
use zip::ZipArchive;

pub(crate) mod errors;

use crate::{
    device::DeviceClient, devices_collection::SingleDevice, errors::DeviceInstallerError,
    RecursiveFind,
};

const PKG_PATH: &str = "PublicStaging"; // The remote directory path where packages will be uploaded
const IPCC_REMOTE_FOLDER: &str = "rsmobiledevice.ipcc"; // Folder for IPCC packages. IPCC packages
                                                        // needs to be uploaded as a folder
const IPA_REMOTE_FILE: &str = "rsmobiledevice.ipa";

/// Struct for managing the installation of iOS packages
///
/// # Type Parameters
/// - `T`: A marker type that determines whether the installer is targeting a single device or a group of devices.
///
#[derive(Debug)]
pub struct DeviceInstaller<'a, T> {
    device: &'a DeviceClient<T>,
    _p: PhantomData<T>, // A phantom type parameter to indicate the type of the target device (single or multiple)
}

/// Enum to represent the type of package being installed.
enum PackageType {
    Ipcc, // Carrier bundle package
    Ipa,  // iOS app package
    Unknown,
}

impl Display for PackageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageType::Ipa => write!(f, "ipa"),
            PackageType::Ipcc => write!(f, "ipcc"),
            PackageType::Unknown => write!(f, "unknown"),
        }
    }
}

impl DeviceInstaller<'_, SingleDevice> {
    /// Installs a package from a given file path.
    ///
    /// # Parameters
    /// - `package_path`: Path to the package to be installed.
    /// - `options`: Optional installation options.
    ///
    /// This method reads the package from the provided path, determines its type, uploads it to the device, and installs it.
    pub fn install_from_path<S>(
        &self,
        package_path: &S,
        options: Option<HashMap<&str, &str>>,
    ) -> Result<(), DeviceInstallerError>
    where
        S: AsRef<OsStr> + ?Sized,
    {
        self.device.check_connected::<DeviceInstallerError>()?;

        let mut file = std::fs::File::open(Path::new(package_path.as_ref()))?;
        let mut file_content = Vec::new();

        file.read_to_end(&mut file_content).unwrap_or_default();

        let mut cursor = Cursor::new(file_content);

        self._install_package(&mut cursor, options, None)
    }

    /// Installs a package from a given file path with a callback of the progress.
    ///
    /// # Parameters
    /// - `package_path`: Path to the package to be installed.
    /// - `options`: Optional installation options.
    ///
    /// This method reads the package from the provided path, determines its type, uploads it to the device, and installs it.
    pub fn install_from_path_with_callback<S, F>(
        &self,
        package_path: &S,
        options: Option<HashMap<&str, &str>>,
        callback: F,
    ) -> Result<(), DeviceInstallerError>
    where
        S: AsRef<OsStr> + ?Sized,
        F: Fn(CommandPlist, StatusPlist) + Send + Sync + 'static,
    {
        self.device.check_connected::<DeviceInstallerError>()?;

        let mut file = std::fs::File::open(Path::new(package_path.as_ref()))?;
        let mut file_content = Vec::new();

        file.read_to_end(&mut file_content).unwrap_or_default();

        let mut cursor = Cursor::new(file_content);

        self._install_package(&mut cursor, options, Some(Box::new(callback)))
    }

    /// Installs a package from a reader (e.g., bytes from memory or a stream).
    ///
    /// It must be mutable as it will set the cursor to the beginning due to multiple reads
    ///
    /// # Parameters
    /// - `package_file`: A reader containing the package data.
    /// - `options`: Optional installation options.
    ///
    /// This method works similarly to `install_from_path`, but it reads the package directly from a reader.
    pub fn install_from_reader<T: Read + Seek>(
        &self,
        package_file: &mut T,
        options: Option<HashMap<&str, &str>>,
    ) -> Result<(), DeviceInstallerError> {
        self.device.check_connected::<DeviceInstallerError>()?;
        self._install_package(package_file, options, None)
    }

    /// Installs a package from a reader (e.g., bytes from memory or a stream) with a progress callback.
    ///
    /// It must be mutable as it will set the cursor to the beginning due to multiple reads
    ///
    /// # Parameters
    /// - `package_file`: A reader containing the package data.
    /// - `options`: Optional installation options.
    ///
    /// This method works similarly to `install_from_path`, but it reads the package directly from a reader.
    pub fn install_from_reader_with_callback<T: Read + Seek, F>(
        &self,
        package_file: &mut T,
        options: Option<HashMap<&str, &str>>,
        callback: F,
    ) -> Result<(), DeviceInstallerError>
    where
        F: Fn(CommandPlist, StatusPlist) + Send + Sync + 'static,
    {
        self.device.check_connected::<DeviceInstallerError>()?;
        self._install_package(package_file, options, Some(Box::new(callback)))
    }

    fn _install_package<T: Read + Seek>(
        &self,
        file: &mut T,
        options: Option<HashMap<&str, &str>>,
        callback: Option<Box<dyn Fn(CommandPlist, StatusPlist) + Send + Sync>>,
    ) -> Result<(), DeviceInstallerError> {
        let device = self.device.get_device();
        let afc_client = self.device.get_afc_client::<DeviceInstallerError>()?;

        self.check_or_create_path(&afc_client, PKG_PATH)?;

        let package_type = self.determine_file_package_type(file)?;

        let mut package_options = InstProxyClient::client_options_new();

        if let Some(dict) = options {
            for (key, value) in dict {
                package_options.dict_set_item(key, value.into())?;
            }
        }

        self.upload_package(&afc_client, &package_type, file)?;

        let installation_client =
            Arc::new(device.new_instproxy_client("rsmobiledevice-deviceinstaller")?);

        let completed = Arc::new(AtomicBool::new(false));
        let completed_clone = Arc::clone(&completed);

        let remote_packages_path = match package_type {
            PackageType::Ipcc => {
                package_options.dict_set_item("PackageType", "CarrierBundle".into())?;
                format!("/{}/{}", PKG_PATH, IPCC_REMOTE_FOLDER)
            }
            PackageType::Ipa => {
                let bundle_id = self.get_bundle_id(file)?;
                package_options.dict_set_item("CFBundleIdentifier", bundle_id.into())?;
                format!("/{}/{}", PKG_PATH, IPA_REMOTE_FILE)
            }
            PackageType::Unknown => return Err(DeviceInstallerError::UnknownPackage),
        };

        installation_client.install_with_callback(
            remote_packages_path,
            Some(package_options),
            Some(Box::new(move |cmd: CommandPlist, status: StatusPlist| {
                if let Some(ref cb) = callback {
                    // this would keep the client in scope and thus continue with the callback
                    //
                    // we would only stop if there is a key named `Error`
                    // or the `PercentComplete` is `100`
                    // or the `Status` is `Complete` ( for ipcc packages )
                    let condition = status.rfind("Error").is_some()
                        || status.rfind("PercentComplete").is_some_and(|n| &n == "100")
                        || status.rfind("Status").is_some_and(|s| &s == "Complete");

                    if condition {
                        completed_clone.store(true, Ordering::SeqCst);
                    }

                    cb(cmd, status); // Execute the callback
                }
            })),
        )?;

        // Wait for the callback to signal completion
        while !completed.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        Ok(())
    }

    /// Determines the type of package based on its content (IPA or IPCC).
    fn determine_file_package_type<T: Read + Seek>(
        &self,
        package: &mut T,
    ) -> Result<PackageType, DeviceInstallerError> {
        let mut archive = ZipArchive::new(package)?;

        let mut package_type = PackageType::Unknown;

        let inside_file = archive.by_index(1)?;

        let file_path_string = inside_file
            .enclosed_name()
            .unwrap_or_default()
            .to_str()
            .unwrap_or_default()
            .to_owned();

        let splitted_file_path: Vec<&str> = file_path_string.split("/").collect();

        for path in splitted_file_path {
            if path.ends_with(".bundle") {
                package_type = PackageType::Ipcc;
            } else if path.ends_with(".app") {
                package_type = PackageType::Ipa;
            }
        }

        Ok(package_type)
    }

    /// Uploads the package to the device using AFC (Apple File Client).
    fn upload_package<T: Read + Seek>(
        &self,
        afc_client: &AfcClient<'_>,
        package_type: &PackageType,
        package: &mut T,
    ) -> Result<(), DeviceInstallerError> {
        match package_type {
            PackageType::Ipcc => {
                self.check_or_create_path(
                    afc_client,
                    &format!("/{}/{}", PKG_PATH, IPCC_REMOTE_FOLDER),
                )?;
                self.upload_ipcc_files(afc_client, package)?;
            }
            PackageType::Ipa => self.upload_ipa_package(afc_client, package)?,
            PackageType::Unknown => (),
        }

        Ok(())
    }

    fn upload_ipa_package<T: Read + Seek>(
        &self,
        afc_client: &AfcClient<'_>,
        ipa_file: &mut T,
    ) -> Result<(), DeviceInstallerError> {
        let remote_file_handler = afc_client.file_open(
            format!("/{}/{}", PKG_PATH, IPA_REMOTE_FILE),
            AfcFileMode::WriteOnly,
        )?;
        ipa_file.seek(SeekFrom::Start(0))?; // Ensures the file cursor is at the beginning

        let mut file_bytes = Vec::new();
        ipa_file.read_to_end(&mut file_bytes)?;

        afc_client.file_write(remote_file_handler, file_bytes)?;
        afc_client.file_close(remote_file_handler)?;

        Ok(())
    }

    fn upload_ipcc_files<T: Read + Seek>(
        &self,
        afc_client: &AfcClient<'_>,
        ipcc_file: &mut T,
    ) -> Result<(), DeviceInstallerError> {
        ipcc_file.seek(SeekFrom::Start(0))?; // Resets the cursor for proper reading

        let mut archive = ZipArchive::new(ipcc_file)?;

        for i in 0..archive.len() {
            let mut inside_file = archive.by_index(i)?;
            let current_file_path = format!(
                "/{}/{}/{}",
                PKG_PATH,
                IPCC_REMOTE_FOLDER,
                inside_file
                    .enclosed_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );

            if inside_file.is_dir() {
                afc_client.make_directory(current_file_path)?;
            } else {
                let mut inside_file_bytes = Vec::new();
                inside_file.read_to_end(&mut inside_file_bytes)?;

                let remote_file_handler =
                    afc_client.file_open(current_file_path, AfcFileMode::WriteOnly)?;

                afc_client.file_write(remote_file_handler, inside_file_bytes)?;
            }
        }

        Ok(())
    }

    /// Extracts the bundle ID from the IPA package.
    fn get_bundle_id<T: Read + Seek>(&self, file: &mut T) -> Result<String, DeviceInstallerError> {
        let mut zip_file = ZipArchive::new(file)?;

        let mut bundle_id = String::new();

        for i in 0..zip_file.len() {
            let mut file = zip_file.by_index(i)?;

            let inner_file_path = match file.enclosed_name() {
                Some(path) => path,
                None => continue,
            };

            for path in inner_file_path.iter() {
                if path.to_str() == Some("Info.plist")
                    && inner_file_path.to_str().unwrap_or_default().len() == 3
                {
                    let mut plist_content = String::new();
                    file.read_to_string(&mut plist_content)?;

                    let plist = Plist::from_xml(plist_content)?;
                    if let Ok(bid) = plist
                        .dict_get_item("CFBundleIdentifier")
                        .and_then(|p| p.get_string_val())
                    {
                        bundle_id = bid;
                    }
                }
            }
        }
        Ok(bundle_id)
    }

    fn check_or_create_path(
        &self,
        afc_client: &AfcClient<'_>,
        path: &str,
    ) -> Result<(), DeviceInstallerError> {
        if afc_client.get_file_info(path).is_ok() {
            Ok(())
        } else {
            afc_client
                .make_directory(path)
                .map_err(|_| DeviceInstallerError::ErrorCreatingFolder)
        }
    }
}

impl<'a, T> DeviceInstaller<'a, T> {
    pub fn new(device: &'a DeviceClient<T>) -> DeviceInstaller<'a, T> {
        DeviceInstaller {
            device,
            _p: PhantomData::<T>,
        }
    }
}
