//! Pure 16×16 RGBA badge icon renderer (no Tauri dependency).
//!
//! Display rules:
//! - count 1–9 → white digit on red rounded badge
//! - count ≥10 → white `9+`
//! - background ~`#EF4444`
//! - glyphs are embedded 5×7 bitmaps (no system fonts)

const SIZE: u32 = 16;
const RED: [u8; 4] = [0xEF, 0x44, 0x44, 0xFF];
const WHITE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];
const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];

/// 5×7 bitmaps; each row is a 5-bit mask (bit 4 = leftmost pixel).
const DIGITS: [[u8; 7]; 10] = [
    // 0
    [
        0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
    ],
    // 1
    [
        0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
    ],
    // 2
    [
        0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
    ],
    // 3
    [
        0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110,
    ],
    // 4
    [
        0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
    ],
    // 5
    [
        0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
    ],
    // 6
    [
        0b01110, 0b10001, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
    ],
    // 7
    [
        0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
    ],
    // 8
    [
        0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
    ],
    // 9
    [
        0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b10001, 0b01110,
    ],
];

const PLUS: [u8; 7] = [
    0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000,
];

/// Render a taskbar overlay badge for `count ≥ 1`.
///
/// Returns `(rgba_bytes, width, height)` suitable for `Image::new_owned`.
pub fn render_badge_icon(count: u32) -> (Vec<u8>, u32, u32) {
    debug_assert!(
        count >= 1,
        "render_badge_icon is only called for count >= 1"
    );

    let mut pixels = vec![0u8; (SIZE * SIZE * 4) as usize];

    // Rounded circular badge background (~#EF4444).
    for y in 0..SIZE {
        for x in 0..SIZE {
            let color = if in_circle(x, y) { RED } else { TRANSPARENT };
            set_pixel(&mut pixels, x, y, color);
        }
    }

    if count >= 10 {
        // "9+" — two 5-wide glyphs with 1px gap, centered in 16px.
        // 5 + 1 + 5 = 11; start_x = (16 - 11) / 2 = 2
        let start_x = 2i32;
        let start_y = 4i32;
        blit_glyph(&mut pixels, start_x, start_y, &DIGITS[9]);
        blit_glyph(&mut pixels, start_x + 6, start_y, &PLUS);
    } else {
        let digit = (count.min(9)) as usize;
        // Center 5×7 glyph: x = (16-5)/2 = 5, y = (16-7)/2 = 4
        blit_glyph(&mut pixels, 5, 4, &DIGITS[digit]);
    }

    (pixels, SIZE, SIZE)
}

fn in_circle(x: u32, y: u32) -> bool {
    let cx = 7.5_f32;
    let cy = 7.5_f32;
    let dx = x as f32 + 0.5 - cx;
    let dy = y as f32 + 0.5 - cy;
    dx * dx + dy * dy <= 7.5 * 7.5
}

fn set_pixel(buf: &mut [u8], x: u32, y: u32, rgba: [u8; 4]) {
    let i = ((y * SIZE + x) * 4) as usize;
    buf[i..i + 4].copy_from_slice(&rgba);
}

fn blit_glyph(buf: &mut [u8], ox: i32, oy: i32, rows: &[u8; 7]) {
    for (row_i, mask) in rows.iter().enumerate() {
        for col in 0..5 {
            if (mask >> (4 - col)) & 1 == 1 {
                let x = ox + col;
                let y = oy + row_i as i32;
                if x >= 0 && y >= 0 && (x as u32) < SIZE && (y as u32) < SIZE {
                    // Only paint white on top of the red badge (or any non-empty pixel area).
                    set_pixel(buf, x as u32, y as u32, WHITE);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_badge_icon_sizes_and_distinct_glyphs() {
        let (b1, w1, h1) = render_badge_icon(1);
        let (b9, w9, h9) = render_badge_icon(9);
        let (b10, w10, h10) = render_badge_icon(10);

        assert_eq!((w1, h1), (16, 16));
        assert_eq!((w9, h9), (16, 16));
        assert_eq!((w10, h10), (16, 16));

        assert_eq!(b1.len(), 16 * 16 * 4);
        assert_eq!(b9.len(), 16 * 16 * 4);
        assert_eq!(b10.len(), 16 * 16 * 4);

        assert!(b1.iter().any(|&c| c != 0), "count=1 icon must be non-zero");
        assert!(b9.iter().any(|&c| c != 0), "count=9 icon must be non-zero");
        assert!(
            b10.iter().any(|&c| c != 0),
            "count=10 icon must be non-zero"
        );

        assert_ne!(b1, b9, "digit 1 and 9 must differ");
        assert_ne!(b1, b10, "digit 1 and 9+ must differ");
        assert_ne!(b9, b10, "digit 9 and 9+ must differ");
    }
}
