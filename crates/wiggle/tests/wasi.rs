use wiggle::{GuestErrorType, GuestPtr, GuestSlice};
use wiggle_test::WasiCtx;

// This test file exists to make sure that the entire `wasi.witx` file can be
// handled by wiggle, producing code that compiles correctly.
// The trait impls here are never executed, and just exist to validate that the
// witx is exposed with the type signatures that we expect.

wiggle::from_witx!({
    witx: ["$CARGO_MANIFEST_DIR/tests/wasi.witx"],
});

// The only test in this file is to verify that the witx document provided by the
// proc macro in the `metadata` module is equal to the document on the disk.
#[test]
fn document_equivalent() {
    let macro_doc = metadata::document();
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push("wasi.witx");
    let disk_doc = witx::load(&[path]).expect("load wasi.witx from disk");

    assert_eq!(macro_doc, disk_doc);
}

type Result<T> = std::result::Result<T, types::Errno>;

impl GuestErrorType for types::Errno {
    fn success() -> types::Errno {
        types::Errno::Success
    }
}

impl<'a> crate::wasi_snapshot_preview1::WasiSnapshotPreview1 for WasiCtx<'a> {
    fn args_get(&mut self, _argv: &GuestPtr<GuestPtr<u8>>, _argv_buf: &GuestPtr<u8>) -> Result<()> {
        unimplemented!("args_get")
    }

    fn args_sizes_get(&mut self) -> Result<(types::Size, types::Size)> {
        unimplemented!("args_sizes_get")
    }

    fn environ_get(
        &mut self,
        _environ: &GuestPtr<GuestPtr<u8>>,
        _environ_buf: &GuestPtr<u8>,
    ) -> Result<()> {
        unimplemented!("environ_get")
    }

    fn environ_sizes_get(&mut self) -> Result<(types::Size, types::Size)> {
        unimplemented!("environ_sizes_get")
    }

    fn clock_res_get(&mut self, _id: types::Clockid) -> Result<types::Timestamp> {
        unimplemented!("clock_res_get")
    }

    fn clock_time_get(
        &mut self,
        _id: types::Clockid,
        _precision: types::Timestamp,
    ) -> Result<types::Timestamp> {
        unimplemented!("clock_time_get")
    }

    fn fd_advise(
        &mut self,
        _fd: types::Fd,
        _offset: types::Filesize,
        _len: types::Filesize,
        _advice: types::Advice,
    ) -> Result<()> {
        unimplemented!("fd_advise")
    }

    fn fd_allocate(
        &mut self,
        _fd: types::Fd,
        _offset: types::Filesize,
        _len: types::Filesize,
    ) -> Result<()> {
        unimplemented!("fd_allocate")
    }

    fn fd_close(&mut self, _fd: types::Fd) -> Result<()> {
        unimplemented!("fd_close")
    }

    fn fd_datasync(&mut self, _fd: types::Fd) -> Result<()> {
        unimplemented!("fd_datasync")
    }

    fn fd_fdstat_get(&mut self, _fd: types::Fd) -> Result<types::Fdstat> {
        unimplemented!("fd_fdstat_get")
    }

    fn fd_fdstat_set_flags(&mut self, _fd: types::Fd, _flags: types::Fdflags) -> Result<()> {
        unimplemented!("fd_fdstat_set_flags")
    }

    fn fd_fdstat_set_rights(
        &mut self,
        _fd: types::Fd,
        _fs_rights_base: types::Rights,
        _fs_rights_inherting: types::Rights,
    ) -> Result<()> {
        unimplemented!("fd_fdstat_set_rights")
    }

    fn fd_filestat_get(&mut self, _fd: types::Fd) -> Result<types::Filestat> {
        unimplemented!("fd_filestat_get")
    }

    fn fd_filestat_set_size(&mut self, _fd: types::Fd, _size: types::Filesize) -> Result<()> {
        unimplemented!("fd_filestat_set_size")
    }

    fn fd_filestat_set_times(
        &mut self,
        _fd: types::Fd,
        _atim: types::Timestamp,
        _mtim: types::Timestamp,
        _fst_flags: types::Fstflags,
    ) -> Result<()> {
        unimplemented!("fd_filestat_set_times")
    }

    fn fd_pread(
        &mut self,
        _fd: types::Fd,
        iovs: &types::IovecArray<'_>,
        _offset: types::Filesize,
    ) -> Result<types::Size> {
        // This is not functional code, but the type annotations demonstrate
        // that we can use the wiggle API to create the datastructures we want
        // for efficient implementation of this function elsewhere.

        let mut slices: Vec<GuestSlice<'_, u8>> = Vec::new();
        for iov_ptr in iovs.iter() {
            let iov_ptr = iov_ptr.expect("iovec element pointer is valid");

            // Borrow checker will make sure the pointee of this read() doesn't overlap with any
            // existing borrows:
            let iov: types::Iovec = iov_ptr.read().expect("read iovec element");
            let base: GuestPtr<u8> = iov.buf;
            let len: u32 = iov.buf_len;
            let buf: GuestPtr<[u8]> = base.as_array(len);
            // GuestSlice will remain borrowed until dropped:
            let slice = buf.as_slice().expect("borrow slice from iovec");
            slices.push(slice);
        }
        println!("iovec slices: [");
        for slice in slices {
            println!("  {:?},", &*slice);
        }
        println!("]");
        unimplemented!("fd_pread")
    }

    fn fd_prestat_get(&mut self, _fd: types::Fd) -> Result<types::Prestat> {
        unimplemented!("fd_prestat_get")
    }

    fn fd_prestat_dir_name(
        &mut self,
        _fd: types::Fd,
        _path: &GuestPtr<u8>,
        _path_len: types::Size,
    ) -> Result<()> {
        unimplemented!("fd_prestat_dir_name")
    }

    fn fd_pwrite(
        &mut self,
        _fd: types::Fd,
        _ciovs: &types::CiovecArray<'_>,
        _offset: types::Filesize,
    ) -> Result<types::Size> {
        unimplemented!("fd_pwrite")
    }

    fn fd_read(&mut self, _fd: types::Fd, _iovs: &types::IovecArray<'_>) -> Result<types::Size> {
        unimplemented!("fd_read")
    }

    fn fd_readdir(
        &mut self,
        _fd: types::Fd,
        _buf: &GuestPtr<u8>,
        _buf_len: types::Size,
        _cookie: types::Dircookie,
    ) -> Result<types::Size> {
        unimplemented!("fd_readdir")
    }

    fn fd_renumber(&mut self, _fd: types::Fd, _to: types::Fd) -> Result<()> {
        unimplemented!("fd_renumber")
    }

    fn fd_seek(
        &mut self,
        _fd: types::Fd,
        _offset: types::Filedelta,
        _whence: types::Whence,
    ) -> Result<types::Filesize> {
        unimplemented!("fd_seek")
    }

    fn fd_sync(&mut self, _fd: types::Fd) -> Result<()> {
        unimplemented!("fd_sync")
    }

    fn fd_tell(&mut self, _fd: types::Fd) -> Result<types::Filesize> {
        unimplemented!("fd_tell")
    }

    fn fd_write(&mut self, _fd: types::Fd, _ciovs: &types::CiovecArray<'_>) -> Result<types::Size> {
        unimplemented!("fd_write")
    }

    fn path_create_directory(&mut self, _fd: types::Fd, _path: &GuestPtr<'_, str>) -> Result<()> {
        unimplemented!("path_create_directory")
    }

    fn path_filestat_get(
        &mut self,
        _fd: types::Fd,
        _flags: types::Lookupflags,
        _path: &GuestPtr<'_, str>,
    ) -> Result<types::Filestat> {
        unimplemented!("path_filestat_get")
    }

    fn path_filestat_set_times(
        &mut self,
        _fd: types::Fd,
        _flags: types::Lookupflags,
        _path: &GuestPtr<'_, str>,
        _atim: types::Timestamp,
        _mtim: types::Timestamp,
        _fst_flags: types::Fstflags,
    ) -> Result<()> {
        unimplemented!("path_filestat_set_times")
    }

    fn path_link(
        &mut self,
        _old_fd: types::Fd,
        _old_flags: types::Lookupflags,
        _old_path: &GuestPtr<'_, str>,
        _new_fd: types::Fd,
        _new_path: &GuestPtr<'_, str>,
    ) -> Result<()> {
        unimplemented!("path_link")
    }

    fn path_open(
        &mut self,
        _fd: types::Fd,
        _dirflags: types::Lookupflags,
        _path: &GuestPtr<'_, str>,
        _oflags: types::Oflags,
        _fs_rights_base: types::Rights,
        _fs_rights_inherting: types::Rights,
        _fdflags: types::Fdflags,
    ) -> Result<types::Fd> {
        unimplemented!("path_open")
    }

    fn path_readlink(
        &mut self,
        _fd: types::Fd,
        _path: &GuestPtr<'_, str>,
        _buf: &GuestPtr<u8>,
        _buf_len: types::Size,
    ) -> Result<types::Size> {
        unimplemented!("path_readlink")
    }

    fn path_remove_directory(&mut self, _fd: types::Fd, _path: &GuestPtr<'_, str>) -> Result<()> {
        unimplemented!("path_remove_directory")
    }

    fn path_rename(
        &mut self,
        _fd: types::Fd,
        _old_path: &GuestPtr<'_, str>,
        _new_fd: types::Fd,
        _new_path: &GuestPtr<'_, str>,
    ) -> Result<()> {
        unimplemented!("path_rename")
    }

    fn path_symlink(
        &mut self,
        _old_path: &GuestPtr<'_, str>,
        _fd: types::Fd,
        _new_path: &GuestPtr<'_, str>,
    ) -> Result<()> {
        unimplemented!("path_symlink")
    }

    fn path_unlink_file(&mut self, _fd: types::Fd, _path: &GuestPtr<'_, str>) -> Result<()> {
        unimplemented!("path_unlink_file")
    }

    fn poll_oneoff(
        &mut self,
        _in_: &GuestPtr<types::Subscription>,
        _out: &GuestPtr<types::Event>,
        _nsubscriptions: types::Size,
    ) -> Result<types::Size> {
        unimplemented!("poll_oneoff")
    }

    fn proc_exit(&mut self, _rval: types::Exitcode) -> wiggle::Trap {
        unimplemented!("proc_exit")
    }

    fn proc_raise(&mut self, _sig: types::Signal) -> Result<()> {
        unimplemented!("proc_raise")
    }

    fn sched_yield(&mut self) -> Result<()> {
        unimplemented!("sched_yield")
    }

    fn random_get(&mut self, _buf: &GuestPtr<u8>, _buf_len: types::Size) -> Result<()> {
        unimplemented!("random_get")
    }

    fn sock_recv(
        &mut self,
        _fd: types::Fd,
        _ri_data: &types::IovecArray<'_>,
        _ri_flags: types::Riflags,
    ) -> Result<(types::Size, types::Roflags)> {
        unimplemented!("sock_recv")
    }

    fn sock_send(
        &mut self,
        _fd: types::Fd,
        _si_data: &types::CiovecArray<'_>,
        _si_flags: types::Siflags,
    ) -> Result<types::Size> {
        unimplemented!("sock_send")
    }

    fn sock_shutdown(&mut self, _fd: types::Fd, _how: types::Sdflags) -> Result<()> {
        unimplemented!("sock_shutdown")
    }
}
