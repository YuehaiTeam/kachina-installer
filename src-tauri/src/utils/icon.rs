use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, SelectObject, BITMAP,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

/// 从 exe 提取图标并转换为 RGBA 字节数组
/// 返回 (rgba_bytes, width, height) 或 None
pub fn extract_icon_from_exe(exe_path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    unsafe {
        // 1. 提取图标
        let path_wide: Vec<u16> = exe_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut large_icon = HICON::default();
        let mut small_icon = HICON::default();

        // 提取第一个图标的大小两种尺寸
        let result = ExtractIconExW(
            PCWSTR(path_wide.as_ptr()),
            0,                     // 索引 0 = 第一个图标
            Some(&mut large_icon), // 大图标（48x48 或更大）
            Some(&mut small_icon), // 小图标（16x16）
            1,                     // 提取 1 个
        );

        if result == 0 {
            tracing::warn!("ExtractIconExW failed to extract icon");
            return None;
        }

        // 优先使用大图标，如果不存在则使用小图标
        let hicon = if !large_icon.is_invalid() {
            // 如果有小图标也要清理
            if !small_icon.is_invalid() {
                let _ = DestroyIcon(small_icon);
            }
            large_icon
        } else if !small_icon.is_invalid() {
            small_icon
        } else {
            tracing::warn!("No valid icon extracted");
            return None;
        };

        // 2. 获取图标信息
        let mut icon_info = ICONINFO::default();
        if GetIconInfo(hicon, &mut icon_info).is_err() {
            let _ = DestroyIcon(hicon);
            return None;
        }

        let hbm_color = icon_info.hbmColor;
        if hbm_color.is_invalid() {
            tracing::warn!("Icon has no color bitmap");
            let _ = DeleteObject(icon_info.hbmMask.into());
            let _ = DestroyIcon(hicon);
            return None;
        }

        // 3. 获取位图尺寸
        let mut bitmap = BITMAP::default();
        if GetObjectW(
            hbm_color.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bitmap as *mut BITMAP as *mut _),
        ) == 0
        {
            // 清理资源
            let _ = DeleteObject(hbm_color.into());
            let _ = DeleteObject(icon_info.hbmMask.into());
            let _ = DestroyIcon(hicon);
            return None;
        }

        let width = bitmap.bmWidth as u32;
        let height = bitmap.bmHeight as u32;

        // 4. 创建设备上下文
        let hdc = CreateCompatibleDC(None);
        if hdc.is_invalid() {
            let _ = DeleteObject(hbm_color.into());
            let _ = DeleteObject(icon_info.hbmMask.into());
            let _ = DestroyIcon(hicon);
            return None;
        }

        let old_bitmap = SelectObject(hdc, hbm_color.into());

        // 5. 准备 BITMAPINFO
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32), // 负值表示自顶向下
                biPlanes: 1,
                biBitCount: 32, // 32位 BGRA
                biCompression: BI_RGB.0,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [Default::default(); 1],
        };

        // 6. 提取 BGRA 数据
        let pixel_count = (width * height) as usize;
        let mut bgra_data: Vec<u8> = vec![0; pixel_count * 4];

        let result = GetDIBits(
            hdc,
            hbm_color,
            0,
            height,
            Some(bgra_data.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        // 7. 清理资源
        SelectObject(hdc, old_bitmap);
        let _ = DeleteDC(hdc);
        let _ = DeleteObject(hbm_color.into());
        let _ = DeleteObject(icon_info.hbmMask.into());
        let _ = DestroyIcon(hicon);

        if result == 0 {
            tracing::warn!("GetDIBits failed");
            return None;
        }

        // 8. 转换 BGRA 到 RGBA
        let rgba_data = bgra_to_rgba(bgra_data);

        Some((rgba_data, width, height))
    }
}

/// 将 BGRA 转换为 RGBA
fn bgra_to_rgba(bgra: Vec<u8>) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(bgra.len());

    for chunk in bgra.chunks_exact(4) {
        rgba.push(chunk[2]); // R (从 B 位置)
        rgba.push(chunk[1]); // G (保持不变)
        rgba.push(chunk[0]); // B (从 R 位置)
        rgba.push(chunk[3]); // A (保持不变)
    }

    rgba
}

/// 提取当前 exe 图标并返回 Tauri Image
/// 失败时返回 None（会记录警告日志）
pub fn get_exe_icon_for_tauri() -> Option<tauri::image::Image<'static>> {
    let exe_path = std::env::current_exe().ok()?;

    match extract_icon_from_exe(&exe_path) {
        Some((rgba_data, width, height)) => {
            tracing::info!("Successfully extracted icon: {}x{}", width, height);
            // Leak the data to get 'static lifetime
            let rgba_static: &'static [u8] = Box::leak(rgba_data.into_boxed_slice());
            Some(tauri::image::Image::new(rgba_static, width, height))
        }
        None => {
            tracing::warn!("Failed to extract icon from exe, using default");
            None
        }
    }
}
