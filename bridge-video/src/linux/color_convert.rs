//! BGRA to NV12 color conversion for V4L2 encoder input

/// Convert a BGRA frame to NV12 format.
///
/// NV12 layout:
///   - Y  plane : `width * height` bytes (one luma sample per pixel)
///   - UV plane : `width * height / 2` bytes (interleaved Cb/Cr, 2×2 sub-sampled)
///
/// Total output size: `width * height * 3 / 2` bytes.
///
/// The conversion uses the BT.601 studio-swing formula.
pub fn bgra_to_nv12(bgra: &[u8], width: u32, height: u32, nv12: &mut Vec<u8>) {
    let w = width as usize;
    let h = height as usize;
    nv12.resize(w * h * 3 / 2, 0);

    let (y_plane, uv_plane) = nv12.split_at_mut(w * h);

    for row in 0..h {
        for col in 0..w {
            let bgra_idx = (row * w + col) * 4;
            let b = bgra[bgra_idx] as i32;
            let g = bgra[bgra_idx + 1] as i32;
            let r = bgra[bgra_idx + 2] as i32;

            // BT.601 conversion
            let y = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            y_plane[row * w + col] = y.clamp(0, 255) as u8;

            if row % 2 == 0 && col % 2 == 0 {
                let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
                let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
                let uv_idx = (row / 2) * w + col;
                uv_plane[uv_idx] = u.clamp(0, 255) as u8;
                uv_plane[uv_idx + 1] = v.clamp(0, 255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nv12_output_size() {
        let bgra = vec![0u8; 8 * 8 * 4];
        let mut nv12 = Vec::new();
        bgra_to_nv12(&bgra, 8, 8, &mut nv12);
        assert_eq!(nv12.len(), 8 * 8 * 3 / 2); // 96 bytes
    }

    #[test]
    fn test_black_pixel_y_value() {
        let bgra = vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
        let mut nv12 = Vec::new();
        bgra_to_nv12(&bgra, 2, 2, &mut nv12);
        assert_eq!(nv12[0], 16); // Y for black = 16 in BT.601
    }

    #[test]
    fn test_white_pixel_y_value() {
        let bgra = vec![
            255, 255, 255, 255, 255, 255, 255, 255,
            255, 255, 255, 255, 255, 255, 255, 255,
        ];
        let mut nv12 = Vec::new();
        bgra_to_nv12(&bgra, 2, 2, &mut nv12);
        assert_eq!(nv12[0], 235); // Y for white = 235 in BT.601
    }
}
