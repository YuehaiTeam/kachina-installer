#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
// binding.rs 由 bindgen 生成，memcmp/memcpy 等 CRT 符号声明用 c_ulonglong；
// x86_64 Windows 上与 stdlib 期望的 usize ABI 一致。
#![allow(suspicious_runtime_symbol_definitions)]

use std::ffi::c_void;
use std::io::{self, Read, Seek, SeekFrom, Write};

include!("../binding.rs");

/// `patch_types.h` 的 `hpatch_kNullStreamPos`（`~(hpatch_StreamPos_t)0`）；bindgen 不展开带类型转换的宏。
const hpatch_kNullStreamPos: hpatch_StreamPos_t = hpatch_StreamPos_t::MAX;

/// `patch_single_stream` 不分配内存，读出补丁头后经此回调向调用方要工作区：
/// `stepMemSize` 之外的部分被三等分，分别作 diff 读取块、old 读取步长与输出写缓冲。
/// 每份取上游推荐的文件 I/O 粒度 `hpatch_kFileIOBufBetterSize`（64 KiB）。
extern "C" fn on_diff_info(
    listener: *mut sspatch_listener_t,
    info: *const hpatch_singleCompressedDiffInfo,
    _out_decompress_plugin: *mut *mut hpatch_TDecompress,
    out_temp_cache: *mut *mut u8,
    out_temp_cache_end: *mut *mut u8,
) -> i32 {
    let listener = unsafe { &mut *listener };
    let info = unsafe { &*info };
    let cache = unsafe { &mut *(listener.import as *mut Vec<u8>) };
    let size = info.stepMemSize as usize + hpatch_kFileIOBufBetterSize as usize * 3;
    *cache = vec![0u8; size];
    // Vec 的堆内存在 cache 被释放前不会移动，patch 期间指针一直有效
    unsafe {
        *out_temp_cache = cache.as_mut_ptr();
        *out_temp_cache_end = cache.as_mut_ptr().add(size);
    }
    1
}

trait ReadSeek: Read + Seek {}

impl<T: Read + Seek> ReadSeek for T {}

/// 回调里出错时只能向 C 返回失败；首个 I/O 错误留在这里，由 Rust 侧在 patch 返回后取出。
struct ReadSeekStreamWrapper<'a> {
    stream: &'a mut dyn ReadSeek,
    /// 当前文件游标；与本次读取起点相同时省掉 seek。出错后置空。
    pos: Option<u64>,
    error: Option<io::Error>,
}
struct ReadStreamWrapper<'a> {
    stream: &'a mut dyn Read,
    error: Option<io::Error>,
}
struct WriteStreamWrapper<'a> {
    stream: &'a mut dyn Write,
    error: Option<io::Error>,
}

/// 记下首个错误，向 C 返回失败。
fn fail(slot: &mut Option<io::Error>, err: io::Error) -> i32 {
    slot.get_or_insert(err);
    0
}

extern "C" fn read_seek_callback(
    stream: *const hpatch_TStreamInput,
    read_from: u64,
    out_data: *mut u8,
    out_data_end: *mut u8,
) -> i32 {
    let read_size = unsafe { out_data_end.offset_from(out_data) } as usize;
    let stream = unsafe { &*stream };
    let wrapper = unsafe { &mut *(stream.streamImport as *mut ReadSeekStreamWrapper) };
    if wrapper.pos != Some(read_from) {
        if let Err(err) = wrapper.stream.seek(SeekFrom::Start(read_from)) {
            wrapper.pos = None;
            return fail(&mut wrapper.error, err);
        }
    }
    let buffer = unsafe { std::slice::from_raw_parts_mut(out_data, read_size) };
    if let Err(err) = wrapper.stream.read_exact(buffer) {
        wrapper.pos = None;
        return fail(&mut wrapper.error, err);
    }
    wrapper.pos = Some(read_from + read_size as u64);
    read_size as i32
}

extern "C" fn read_seq_callback(
    stream: *const hpatch_TStreamInput,
    _read_from: u64,
    out_data: *mut u8,
    out_data_end: *mut u8,
) -> i32 {
    let read_size = unsafe { out_data_end.offset_from(out_data) } as usize;
    let stream = unsafe { &*stream };
    let wrapper = unsafe { &mut *(stream.streamImport as *mut ReadStreamWrapper) };
    let buffer = unsafe { std::slice::from_raw_parts_mut(out_data, read_size) };
    if let Err(err) = wrapper.stream.read_exact(buffer) {
        return fail(&mut wrapper.error, err);
    }
    read_size as i32
}

extern "C" fn write_seq_callback(
    stream: *const hpatch_TStreamOutput,
    _write_to: u64,
    out_data: *const u8,
    out_data_end: *const u8,
) -> i32 {
    let write_size = unsafe { out_data_end.offset_from(out_data) } as usize;
    let stream = unsafe { &*stream };
    let wrapper = unsafe { &mut *(stream.streamImport as *mut WriteStreamWrapper) };
    let buffer = unsafe { std::slice::from_raw_parts(out_data, write_size) };
    if let Err(err) = wrapper.stream.write_all(buffer) {
        return fail(&mut wrapper.error, err);
    }
    write_size as i32
}

/// 把单流补丁 `diff` 应用到 `input`（旧文件，需随机读），结果顺序写入 `output`。
///
/// 任一流回调出错时返回该 `io::Error`（调用方的读写器报出的错误原样带回，例如取消）；
/// 否则 HPatch 返回失败码时报 `io::ErrorKind::Other`，消息含失败码。
pub fn safe_patch_single_stream(
    mut output: impl Write,
    mut diff: impl Read,
    diff_size: usize,
    mut input: impl Read + Seek,
    input_size: usize,
) -> io::Result<()> {
    let mut cache: Vec<u8> = Vec::new();
    let mut listener = sspatch_listener_t {
        import: &mut cache as *mut Vec<u8> as *mut c_void,
        onDiffInfo: Some(on_diff_info),
        onPatchFinish: None,
    };
    let mut input_wrapper = ReadSeekStreamWrapper {
        stream: &mut input,
        pos: None,
        error: None,
    };
    let mut diff_wrapper = ReadStreamWrapper {
        stream: &mut diff,
        error: None,
    };
    let mut output_wrapper = WriteStreamWrapper {
        stream: &mut output,
        error: None,
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
        // 输出长度不设上限；patch 以补丁头里的 newDataSize 为准
        streamSize: hpatch_kNullStreamPos,
        streamImport: &mut output_wrapper as *mut WriteStreamWrapper as *mut c_void,
        write: Some(write_seq_callback),
        read_writed: None,
    };
    let res: i32 = unsafe {
        patch_single_stream(
            &mut listener,
            &mut stream_output,
            &mut stream_input,
            &mut diff_input,
            0,
            std::ptr::null_mut(),
        )
    };
    if let Some(err) = diff_wrapper
        .error
        .or(input_wrapper.error)
        .or(output_wrapper.error)
    {
        return Err(err);
    }
    if res != 1 {
        return Err(io::Error::other(format!("hpatch failed with code {res}")));
    }
    Ok(())
}
