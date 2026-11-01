// The app's icon, drawn from code at any size: a small quilt of file-sized
// patches. The build script includes this file to put the same picture on the
// .exe, so the window and the file can't drift apart.

/// `(x0, y0, x1, y1, colour)` on a 64-unit square.
const PATCHES: [(usize, usize, usize, usize, [u8; 3]); 6] = [
    (2, 2, 38, 62, [44, 190, 190]),
    (40, 2, 62, 26, [229, 72, 96]),
    (40, 28, 62, 44, [240, 190, 66]),
    (40, 46, 50, 62, [150, 200, 72]),
    (52, 46, 62, 54, [118, 110, 240]),
    (52, 56, 62, 62, [176, 96, 226]),
];

/// `size` by `size` RGBA pixels, top row first.
pub fn pixels(size: usize) -> Vec<u8> {
    let mut rgba = vec![0_u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let (ux, uy) = (x * 64 / size, y * 64 / size);
            let patch = PATCHES
                .iter()
                .find(|p| ux >= p.0 && ux < p.2 && uy >= p.1 && uy < p.3);
            if let Some(&(x0, y0, x1, y1, colour)) = patch {
                // Each patch is lit from the top left, as tiles are on the map.
                let across = (ux - x0) as f32 / (x1 - x0) as f32;
                let down = (uy - y0) as f32 / (y1 - y0) as f32;
                let light = 1.08 - 0.22 * (across + down) / 2.0;
                let at = (y * size + x) * 4;
                for i in 0..3 {
                    rgba[at + i] = (f32::from(colour[i]) * light).min(255.0) as u8;
                }
                rgba[at + 3] = 255;
            }
        }
    }
    rgba
}
