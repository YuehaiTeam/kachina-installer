#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::ffi::c_void;

include!("../binding.rs");

trait WriteSeek: std::io::Write + std::io::Seek {}

impl<T: std::io::Write + std::io::Seek> WriteSeek for T {}

struct WriteStreamWrapper<'a> {
    stream: &'a mut dyn WriteSeek,
}

extern "C" fn write_seek_callback(
    stream: *const hpatch_TStreamOutput,
    write_to: u64,
    out_data: *const u8,
    out_data_end: *const u8,
) -> i32 {
    let write_size = unsafe { out_data_end.offset_from(out_data) };
    let stream: &hpatch_TStreamOutput = unsafe { &*stream };
    let input_wrapper = unsafe { &mut *(stream.streamImport as *mut WriteStreamWrapper) };
    // seek
    if let Err(err) = input_wrapper
        .stream
        .seek(std::io::SeekFrom::Start(write_to))
    {
        println!("Error in read_seek: {:?}", err);
        return 0;
    }
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

pub fn safe_create_single_patch(
    new_data: &[u8],
    old_data: &[u8],
    mut output: impl std::io::Write + std::io::Seek,
    level: u8,
) -> Result<(), String> {
    let new_start_ptr = new_data.as_ptr();
    let new_end_ptr = unsafe { new_start_ptr.add(new_data.len()) };
    let old_start_ptr = old_data.as_ptr();
    let old_end_ptr = unsafe { old_start_ptr.add(old_data.len()) };
    let mut output_wrapper = WriteStreamWrapper {
        stream: &mut output,
    };
    let mut stream_output = hpatch_TStreamOutput {
        // 1G
        streamSize: 1 << 30,
        streamImport: &mut output_wrapper as *mut WriteStreamWrapper as *mut c_void,
        write: Some(write_seek_callback),
        read_writed: None,
    };
    unsafe {
        create_single_compressed_diff(
            new_start_ptr,
            new_end_ptr,
            old_start_ptr,
            old_end_ptr,
            &mut stream_output,
            std::ptr::null_mut(),
            level as i32,
            1024 * 256,
            true,
            std::ptr::null_mut(),
            1,
        );
    }
    Ok(())
}
