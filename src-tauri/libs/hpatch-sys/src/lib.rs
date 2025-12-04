#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::{ffi::c_void, mem::ManuallyDrop};

include!("../binding.rs");

extern "C" fn on_diff_info(
    listener: *mut sspatch_listener_t,
    _info: *const hpatch_singleCompressedDiffInfo,
    _out_decompress_plugin: *mut *mut hpatch_TDecompress,
    out_temp_cache: *mut *mut u8,
    out_temp_cache_end: *mut *mut u8,
) -> i32 {
    let listener = unsafe { &mut *listener };
    let info = unsafe { &*(_info as *const hpatch_singleCompressedDiffInfo) };
    let buffer_size = info.stepMemSize as usize + hpatch_kStreamCacheSize as usize * 3;
    let mut buffer = ManuallyDrop::new(vec![0u8; buffer_size]);
    let buffer_info = unsafe { &mut *(listener.import as *mut BufferInfo) };
    let buffer_slice = buffer.as_mut_ptr();
    let buffer_end = unsafe { buffer_slice.add(buffer_size) };
    buffer_info.buffer = Some(buffer);
    buffer_info.buffer_size = buffer_size;
    unsafe {
        *out_temp_cache = buffer_slice;
        *out_temp_cache_end = buffer_end;
    }
    1
}

pub struct BufferInfo {
    buffer: Option<ManuallyDrop<Vec<u8>>>,
    buffer_size: usize,
}

impl sspatch_listener_t {
    pub fn new_dummy(buffer: &mut BufferInfo) -> sspatch_listener_t {
        sspatch_listener_t {
            import: buffer as *mut BufferInfo as *mut c_void,
            onDiffInfo: Some(on_diff_info),
            onPatchFinish: None,
        }
    }
}

trait ReadSeek: std::io::Read + std::io::Seek {}

impl<T: std::io::Read + std::io::Seek> ReadSeek for T {}

struct ReadSeekStreamWrapper<'a> {
    stream: &'a mut dyn ReadSeek,
}
struct ReadStreamWrapper<'a> {
    stream: &'a mut dyn std::io::Read,
}
struct WriteStreamWrapper<'a> {
    stream: &'a mut dyn std::io::Write,
}

extern "C" fn read_seek_callback(
    stream: *const hpatch_TStreamInput,
    read_from: u64,
    out_data: *mut u8,
    out_data_end: *mut u8,
) -> i32 {
    let read_size = unsafe { out_data_end.offset_from(out_data) };
    let stream = unsafe { &*stream };
    let input_wrapper = unsafe { &mut *(stream.streamImport as *mut ReadSeekStreamWrapper) };
    // seek
    if let Err(err) = input_wrapper
        .stream
        .seek(std::io::SeekFrom::Start(read_from))
    {
        println!("Error in read_seek: {:?}", err);
        return 0;
    }
    // buffer: out_data to out_data_end
    let buffer = unsafe { std::slice::from_raw_parts_mut(out_data, read_size as usize) };
    // read exact, return 0 if failed
    let res = input_wrapper.stream.read_exact(buffer);
    if let Err(err) = res {
        println!("Error in read_seek_callback: {:?}", err);
        return 0;
    }
    read_size as i32
}
extern "C" fn read_seq_callback(
    stream: *const hpatch_TStreamInput,
    _read_from: u64,
    out_data: *mut u8,
    out_data_end: *mut u8,
) -> i32 {
    let read_size = unsafe { out_data_end.offset_from(out_data) };
    let stream = unsafe { &*stream };
    let input_wrapper = unsafe { &mut *(stream.streamImport as *mut ReadStreamWrapper) };
    // buffer: out_data to out_data_end
    let buffer = unsafe { std::slice::from_raw_parts_mut(out_data, read_size as usize) };
    // read exact, return 0 if failed
    let res = input_wrapper.stream.read_exact(buffer);
    if let Err(err) = res {
        println!("Error in read_seq_callback: {:?}", err);
        return 0;
    }
    read_size as i32
}
extern "C" fn write_seq_callback(
    stream: *const hpatch_TStreamOutput,
    _write_to: u64,
    out_data: *const u8,
    out_data_end: *const u8,
) -> i32 {
    let write_size = unsafe { out_data_end.offset_from(out_data) };
    let stream: &hpatch_TStreamOutput = unsafe { &*stream };
    let input_wrapper = unsafe { &mut *(stream.streamImport as *mut WriteStreamWrapper) };
    // buffer: out_data to out_data_end
    let buffer = unsafe { std::slice::from_raw_parts(out_data, write_size as usize) };
    // read exact, return 0 if failed
    let res = input_wrapper.stream.write_all(buffer);
    if let Err(err) = res {
        println!("Error in write_seq_callback: {:?}", err);
        return 0;
    }
    write_size as i32
}

pub fn safe_patch_single_stream(
    mut output: impl std::io::Write,
    mut diff: impl std::io::Read,
    diff_size: usize,
    mut input: impl std::io::Read + std::io::Seek,
    input_size: usize,
) -> i32 {
    // 10k buffer
    let mut buffer_info = BufferInfo {
        buffer: None,
        buffer_size: 0,
    };
    let mut listener = sspatch_listener_t::new_dummy(&mut buffer_info);
    let listener_ptr = &mut listener as *mut sspatch_listener_t;
    let coverlistener_ptr = std::ptr::null_mut();
    let mut input_wrapper = ReadSeekStreamWrapper { stream: &mut input };
    let mut diff_wrapper = ReadStreamWrapper { stream: &mut diff };
    let mut output_wrapper = WriteStreamWrapper {
        stream: &mut output,
    };
    let mut stream_input = hpatch_TStreamInput {
        streamSize: input_size as u64,
        _private_reserved: std::ptr::null_mut(),
        streamImport: &mut input_wrapper as *mut ReadSeekStreamWrapper as *mut c_void,
        read: Some(read_seek_callback),
    };
    let mut diff_input = hpatch_TStreamInput {
        streamSize: diff_size as u64,
        _private_reserved: std::ptr::null_mut(),
        streamImport: &mut diff_wrapper as *mut ReadStreamWrapper as *mut c_void,
        read: Some(read_seq_callback),
    };
    let mut stream_output = hpatch_TStreamOutput {
        // 1G
        streamSize: 1 << 30,
        streamImport: &mut output_wrapper as *mut WriteStreamWrapper as *mut c_void,
        write: Some(write_seq_callback),
        read_writed: None,
    };
    let res: i32 = unsafe {
        patch_single_stream(
            listener_ptr,
            &mut stream_output as *mut hpatch_TStreamOutput,
            &mut stream_input as *mut hpatch_TStreamInput,
            &mut diff_input as *mut hpatch_TStreamInput,
            0,
            coverlistener_ptr,
        )
    };
    if buffer_info.buffer.is_some() {
        let mut buffer = buffer_info.buffer.take().unwrap();
        unsafe {
            ManuallyDrop::drop(&mut buffer);
        }
    }
    res
}
