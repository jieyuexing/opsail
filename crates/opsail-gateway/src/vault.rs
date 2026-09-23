use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use age::secrecy::SecretString;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{Connection, ConnectionSummary, GatewayError, Secret, client::validate_connection};

const MAX_VAULT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Contents {
    schema_version: u8,
    connections: BTreeMap<String, Connection>,
}

#[derive(Debug, Clone)]
pub struct Vault {
    directory: PathBuf,
}

pub fn default_data_dir() -> Result<PathBuf, GatewayError> {
    directories::BaseDirs::new()
        .map(|dirs| dirs.config_dir().join("opsail").join("gateway"))
        .ok_or_else(|| {
            GatewayError::vault(
                "data-directory-unavailable",
                "specify an explicit data directory",
            )
        })
}

impl Vault {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
    pub fn path(&self) -> PathBuf {
        self.directory.join("vault.age")
    }

    pub fn init(&self, passphrase: &Secret) -> Result<(), GatewayError> {
        check_password(passphrase)?;
        let _lock = self.lock(true)?;
        if self.path().try_exists().map_err(io_error)? {
            return Err(GatewayError::vault(
                "vault-exists",
                "vault already exists; use rekey to change its password",
            ));
        }
        self.write(
            &Contents {
                schema_version: 1,
                connections: BTreeMap::new(),
            },
            passphrase,
        )
    }

    pub fn list(&self, passphrase: &Secret) -> Result<Vec<ConnectionSummary>, GatewayError> {
        let _lock = self.lock(false)?;
        Ok(self
            .read(passphrase)?
            .connections
            .values()
            .map(ConnectionSummary::from)
            .collect())
    }

    pub fn get(&self, passphrase: &Secret, name: &str) -> Result<Connection, GatewayError> {
        let _lock = self.lock(false)?;
        self.read(passphrase)?
            .connections
            .remove(name)
            .ok_or_else(missing_connection)
    }

    pub fn set(
        &self,
        passphrase: &Secret,
        connection: Connection,
    ) -> Result<ConnectionSummary, GatewayError> {
        validate_connection(&connection)?;
        let _lock = self.lock(false)?;
        let mut contents = self.read(passphrase)?;
        if contents.connections.len() >= 128 && !contents.connections.contains_key(&connection.name)
        {
            return Err(GatewayError::vault(
                "connection-limit",
                "vault supports at most 128 connections",
            ));
        }
        let summary = ConnectionSummary::from(&connection);
        contents
            .connections
            .insert(connection.name.clone(), connection);
        self.write(&contents, passphrase)?;
        Ok(summary)
    }

    pub fn remove(&self, passphrase: &Secret, name: &str) -> Result<(), GatewayError> {
        let _lock = self.lock(false)?;
        let mut contents = self.read(passphrase)?;
        contents
            .connections
            .remove(name)
            .ok_or_else(missing_connection)?;
        self.write(&contents, passphrase)
    }

    pub fn rekey(&self, old: &Secret, new: &Secret) -> Result<(), GatewayError> {
        check_password(new)?;
        let _lock = self.lock(false)?;
        let contents = self.read(old)?;
        self.write(&contents, new)
    }

    fn lock(&self, create: bool) -> Result<File, GatewayError> {
        if create {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&self.directory).map_err(io_error)?;
        }
        match fs::symlink_metadata(&self.directory) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => (),
            _ => {
                return Err(GatewayError::vault(
                    "vault-unavailable",
                    "vault directory is missing or is not a regular directory",
                ));
            }
        }
        let lock_path = self.directory.join("vault.lock");
        reject_non_file(&lock_path)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(lock_path).map_err(io_error)?;
        file.try_lock_exclusive().map_err(|_| {
            GatewayError::vault(
                "vault-busy",
                "another operation holds the vault lock; retry after it finishes",
            )
        })?;
        Ok(file)
    }

    fn read(&self, passphrase: &Secret) -> Result<Contents, GatewayError> {
        check_password(passphrase)?;
        reject_non_file(&self.path())?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(self.path()).map_err(|_| {
            GatewayError::vault(
                "vault-unavailable",
                "vault is unavailable; initialize it first",
            )
        })?;
        let mut encrypted = Vec::new();
        file.take(MAX_VAULT_BYTES + 1)
            .read_to_end(&mut encrypted)
            .map_err(io_error)?;
        if encrypted.len() as u64 > MAX_VAULT_BYTES {
            return Err(invalid_vault());
        }
        let decryptor = age::Decryptor::new(encrypted.as_slice()).map_err(|_| invalid_vault())?;
        let mut identity =
            age::scrypt::Identity::new(SecretString::from(passphrase.expose().to_owned()));
        identity.set_max_work_factor(18);
        let mut reader = decryptor
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .map_err(|_| unlock_error())?;
        let mut plaintext = Zeroizing::new(Vec::new());
        reader
            .read_to_end(&mut plaintext)
            .map_err(|_| unlock_error())?;
        let contents: Contents = serde_json::from_slice(&plaintext).map_err(|_| invalid_vault())?;
        if contents.schema_version != 1 || contents.connections.len() > 128 {
            return Err(invalid_vault());
        }
        for (name, connection) in &contents.connections {
            if name != &connection.name {
                return Err(invalid_vault());
            }
            validate_connection(connection).map_err(|_| invalid_vault())?;
        }
        Ok(contents)
    }

    fn write(&self, contents: &Contents, passphrase: &Secret) -> Result<(), GatewayError> {
        reject_non_file(&self.path())?;
        let plaintext = Zeroizing::new(serde_json::to_vec(contents).map_err(|_| invalid_vault())?);
        if plaintext.len() > MAX_VAULT_BYTES as usize - 4096 {
            return Err(invalid_vault());
        }
        // Only ciphertext is ever written to the temporary file. NamedTempFile
        // creates owner-only files on Unix; Windows inherits the user's directory ACL.
        let mut temp = tempfile::NamedTempFile::new_in(&self.directory).map_err(io_error)?;
        let mut recipient =
            age::scrypt::Recipient::new(SecretString::from(passphrase.expose().to_owned()));
        // A fixed work factor keeps vaults portable and bounds decryption memory.
        // Tests exercise the same age format with a cheaper work factor.
        recipient.set_work_factor(if cfg!(test) { 10 } else { 18 });
        let encryptor =
            age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
                .map_err(|_| invalid_vault())?;
        let mut writer = encryptor
            .wrap_output(temp.as_file_mut())
            .map_err(io_error)?;
        writer.write_all(&plaintext).map_err(io_error)?;
        writer.finish().map_err(io_error)?;
        temp.as_file().sync_all().map_err(io_error)?;
        temp.persist(self.path()).map_err(|_| {
            GatewayError::vault(
                "vault-write-failed",
                "could not atomically replace the vault",
            )
        })?;
        Ok(())
    }
}

fn reject_non_file(path: &Path) -> Result<(), GatewayError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => Err(GatewayError::vault(
            "unsafe-vault-path",
            "vault and lock paths must be regular files",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}
fn check_password(passphrase: &Secret) -> Result<(), GatewayError> {
    if passphrase.expose().is_empty() || passphrase.expose().len() > 4096 {
        Err(GatewayError::input(
            "passphrase must contain 1 to 4096 bytes",
        ))
    } else {
        Ok(())
    }
}
fn missing_connection() -> GatewayError {
    GatewayError::vault("connection-not-found", "connection does not exist")
}
fn invalid_vault() -> GatewayError {
    GatewayError::vault(
        "invalid-vault",
        "vault is damaged or uses an unsupported schema",
    )
}
fn unlock_error() -> GatewayError {
    GatewayError::vault("vault-unlock-failed", "wrong passphrase or damaged vault")
}
fn io_error(_: std::io::Error) -> GatewayError {
    GatewayError::vault("vault-io-failed", "could not access the vault")
}
