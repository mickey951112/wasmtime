use crate::address_map::{ModuleAddressMap, ValueLabelsRanges};
use crate::compilation::{Compilation, Relocations};
use crate::module::Module;
use crate::module_environ::FunctionBodyData;
use core::hash::Hasher;
use cranelift_codegen::{ir, isa};
use cranelift_entity::PrimaryMap;
use cranelift_wasm::DefinedFuncIndex;
use lazy_static::lazy_static;
use log::{debug, trace, warn};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::string::{String, ToString};

#[macro_use] // for tests
mod config;
mod worker;

use config::{cache_config, CacheConfig};
pub use config::{create_new_config, init};
use worker::{worker, Worker};

lazy_static! {
    static ref SELF_MTIME: String = {
        std::env::current_exe()
            .map_err(|_| warn!("Failed to get path of current executable"))
            .ok()
            .and_then(|path| {
                fs::metadata(&path)
                    .map_err(|_| warn!("Failed to get metadata of current executable"))
                    .ok()
            })
            .and_then(|metadata| {
                metadata
                    .modified()
                    .map_err(|_| warn!("Failed to get metadata of current executable"))
                    .ok()
            })
            .and_then(|mtime| {
                Some(match mtime.duration_since(std::time::UNIX_EPOCH) {
                    Ok(duration) => format!("{}", duration.as_millis()),
                    Err(err) => format!("m{}", err.duration().as_millis()),
                })
            })
            .unwrap_or("no-mtime".to_string())
    };
}

pub struct ModuleCacheEntry<'config, 'worker>(Option<ModuleCacheEntryInner<'config, 'worker>>);

struct ModuleCacheEntryInner<'config, 'worker> {
    mod_cache_path: PathBuf,
    cache_config: &'config CacheConfig,
    worker: &'worker Worker,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct ModuleCacheData {
    compilation: Compilation,
    relocations: Relocations,
    address_transforms: ModuleAddressMap,
    value_ranges: ValueLabelsRanges,
    stack_slots: PrimaryMap<DefinedFuncIndex, ir::StackSlots>,
}

type ModuleCacheDataTupleType = (
    Compilation,
    Relocations,
    ModuleAddressMap,
    ValueLabelsRanges,
    PrimaryMap<DefinedFuncIndex, ir::StackSlots>,
);

struct Sha256Hasher(Sha256);

impl<'config, 'worker> ModuleCacheEntry<'config, 'worker> {
    pub fn new<'data>(
        module: &Module,
        function_body_inputs: &PrimaryMap<DefinedFuncIndex, FunctionBodyData<'data>>,
        isa: &dyn isa::TargetIsa,
        compiler_name: &str,
        generate_debug_info: bool,
    ) -> Self {
        let cache_config = cache_config();
        if cache_config.enabled() {
            Self(Some(ModuleCacheEntryInner::new(
                module,
                function_body_inputs,
                isa,
                compiler_name,
                generate_debug_info,
                cache_config,
                worker(),
            )))
        } else {
            Self(None)
        }
    }

    #[cfg(test)]
    fn from_inner<'data>(inner: ModuleCacheEntryInner<'config, 'worker>) -> Self {
        Self(Some(inner))
    }

    pub fn get_data(&self) -> Option<ModuleCacheData> {
        if let Some(inner) = &self.0 {
            inner.get_data().map(|val| {
                inner.worker.on_cache_get_async(&inner.mod_cache_path); // call on success
                val
            })
        } else {
            None
        }
    }

    pub fn update_data(&self, data: &ModuleCacheData) {
        if let Some(inner) = &self.0 {
            inner.update_data(data).map(|val| {
                inner.worker.on_cache_update_async(&inner.mod_cache_path); // call on success
                val
            });
        }
    }
}

impl<'config, 'worker> ModuleCacheEntryInner<'config, 'worker> {
    fn new<'data>(
        module: &Module,
        function_body_inputs: &PrimaryMap<DefinedFuncIndex, FunctionBodyData<'data>>,
        isa: &dyn isa::TargetIsa,
        compiler_name: &str,
        generate_debug_info: bool,
        cache_config: &'config CacheConfig,
        worker: &'worker Worker,
    ) -> Self {
        let hash = Sha256Hasher::digest(module, function_body_inputs);
        let compiler_dir = if cfg!(debug_assertions) {
            format!(
                "{comp_name}-{comp_ver}-{comp_mtime}",
                comp_name = compiler_name,
                comp_ver = env!("GIT_REV"),
                comp_mtime = *SELF_MTIME,
            )
        } else {
            format!(
                "{comp_name}-{comp_ver}",
                comp_name = compiler_name,
                comp_ver = env!("GIT_REV"),
            )
        };
        let mod_filename = format!(
            "mod-{mod_hash}{mod_dbg}",
            mod_hash = base64::encode_config(&hash, base64::URL_SAFE_NO_PAD), // standard encoding uses '/' which can't be used for filename
            mod_dbg = if generate_debug_info { ".d" } else { "" },
        );
        let mod_cache_path = cache_config
            .directory()
            .join(isa.triple().to_string())
            .join(compiler_dir)
            .join(mod_filename);

        Self {
            mod_cache_path,
            cache_config,
            worker,
        }
    }

    fn get_data(&self) -> Option<ModuleCacheData> {
        trace!("get_data() for path: {}", self.mod_cache_path.display());
        let compressed_cache_bytes = fs::read(&self.mod_cache_path).ok()?;
        let cache_bytes = zstd::decode_all(&compressed_cache_bytes[..])
            .map_err(|err| warn!("Failed to decompress cached code: {}", err))
            .ok()?;
        bincode::deserialize(&cache_bytes[..])
            .map_err(|err| warn!("Failed to deserialize cached code: {}", err))
            .ok()
    }

    fn update_data(&self, data: &ModuleCacheData) -> Option<()> {
        trace!("update_data() for path: {}", self.mod_cache_path.display());
        let serialized_data = bincode::serialize(&data)
            .map_err(|err| warn!("Failed to serialize cached code: {}", err))
            .ok()?;
        let compressed_data = zstd::encode_all(
            &serialized_data[..],
            self.cache_config.baseline_compression_level(),
        )
        .map_err(|err| warn!("Failed to compress cached code: {}", err))
        .ok()?;

        // Optimize syscalls: first, try writing to disk. It should succeed in most cases.
        // Otherwise, try creating the cache directory and retry writing to the file.
        if fs_write_atomic(&self.mod_cache_path, "mod", &compressed_data) {
            return Some(());
        }

        debug!(
            "Attempting to create the cache directory, because \
             failed to write cached code to disk, path: {}",
            self.mod_cache_path.display(),
        );

        let cache_dir = self.mod_cache_path.parent().unwrap();
        fs::create_dir_all(cache_dir)
            .map_err(|err| {
                warn!(
                    "Failed to create cache directory, path: {}, message: {}",
                    cache_dir.display(),
                    err
                )
            })
            .ok()?;

        if fs_write_atomic(&self.mod_cache_path, "mod", &compressed_data) {
            Some(())
        } else {
            None
        }
    }
}

impl ModuleCacheData {
    pub fn from_tuple(data: ModuleCacheDataTupleType) -> Self {
        Self {
            compilation: data.0,
            relocations: data.1,
            address_transforms: data.2,
            value_ranges: data.3,
            stack_slots: data.4,
        }
    }

    pub fn to_tuple(self) -> ModuleCacheDataTupleType {
        (
            self.compilation,
            self.relocations,
            self.address_transforms,
            self.value_ranges,
            self.stack_slots,
        )
    }
}

impl Sha256Hasher {
    pub fn digest<'data>(
        module: &Module,
        function_body_inputs: &PrimaryMap<DefinedFuncIndex, FunctionBodyData<'data>>,
    ) -> [u8; 32] {
        let mut hasher = Self(Sha256::new());
        module.hash_for_cache(function_body_inputs, &mut hasher);
        hasher.0.result().into()
    }
}

impl Hasher for Sha256Hasher {
    fn finish(&self) -> u64 {
        panic!("Sha256Hasher doesn't support finish!");
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0.input(bytes);
    }
}

// Assumption: path inside cache directory.
// Then, we don't have to use sound OS-specific exclusive file access.
// Note: there's no need to remove temporary file here - cleanup task will do it later.
fn fs_write_atomic(path: &Path, reason: &str, contents: &[u8]) -> bool {
    let lock_path = path.with_extension(format!("wip-atomic-write-{}", reason));
    fs::OpenOptions::new()
        .create_new(true) // atomic file creation (assumption: no one will open it without this flag)
        .write(true)
        .open(&lock_path)
        .and_then(|mut file| file.write_all(contents))
        // file should go out of scope and be closed at this point
        .and_then(|()| fs::rename(&lock_path, &path)) // atomic file rename
        .map_err(|err| {
            warn!(
                "Failed to write file with rename, lock path: {}, target path: {}, err: {}",
                lock_path.display(),
                path.display(),
                err
            )
        })
        .is_ok()
}

#[cfg(test)]
mod tests;
