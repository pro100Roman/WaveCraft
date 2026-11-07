use eframe::egui::{self, Rect};
use image::ImageFormat;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingSnapshot {
    pub(crate) plot_rect: Rect,
    pub(crate) pixels_per_point: f32,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SnapshotImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

pub(crate) fn crop_snapshot_image(
    screenshot: &egui::ColorImage,
    plot_rect: Rect,
    pixels_per_point: f32,
) -> Result<SnapshotImage, String> {
    if screenshot.size[0] == 0 || screenshot.size[1] == 0 {
        return Err("the captured image is empty".to_owned());
    }
    if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
        return Err("the display scale is invalid".to_owned());
    }

    let image_width = screenshot.size[0];
    let image_height = screenshot.size[1];
    let left = (plot_rect.left() * pixels_per_point).floor().max(0.0) as usize;
    let top = (plot_rect.top() * pixels_per_point).floor().max(0.0) as usize;
    let right = (plot_rect.right() * pixels_per_point)
        .ceil()
        .clamp(0.0, image_width as f32) as usize;
    let bottom = (plot_rect.bottom() * pixels_per_point)
        .ceil()
        .clamp(0.0, image_height as f32) as usize;
    let left = left.min(image_width);
    let top = top.min(image_height);
    if right <= left || bottom <= top {
        return Err("the waveform plot is outside the captured image".to_owned());
    }

    let width = right - left;
    let height = bottom - top;
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in top..bottom {
        let row_start = y * image_width + left;
        let row_end = row_start + width;
        for pixel in &screenshot.pixels[row_start..row_end] {
            rgba.extend_from_slice(&pixel.to_srgba_unmultiplied());
        }
    }

    Ok(SnapshotImage {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

fn available_snapshot_path(directory: &Path) -> Option<PathBuf> {
    let default_path = directory.join("wavecraft-snapshot.png");
    if !default_path.exists() {
        return Some(default_path);
    }
    (2_u32..=u32::MAX)
        .map(|index| directory.join(format!("wavecraft-snapshot-{index}.png")))
        .find(|path| !path.exists())
}

pub(crate) fn save_snapshot_image(
    directory: &Path,
    snapshot: &SnapshotImage,
) -> Result<PathBuf, String> {
    let path = available_snapshot_path(directory)
        .ok_or_else(|| format!("no free snapshot file name in {}", directory.display()))?;
    image::save_buffer_with_format(
        &path,
        &snapshot.rgba,
        snapshot.width,
        snapshot.height,
        image::ColorType::Rgba8,
        ImageFormat::Png,
    )
    .map_err(|error| format!("could not save {}: {error}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Color32, Pos2};

    #[test]
    fn snapshot_crop_uses_plot_bounds_and_display_scale() {
        let mut screenshot = egui::ColorImage::new([8, 6], Color32::BLACK);
        for (index, pixel) in screenshot.pixels.iter_mut().enumerate() {
            *pixel = Color32::from_rgb(index as u8, 0, 0);
        }

        let snapshot = crop_snapshot_image(
            &screenshot,
            Rect::from_min_max(Pos2::new(1.0, 0.5), Pos2::new(3.0, 2.5)),
            2.0,
        )
        .expect("the plot is inside the screenshot");

        assert_eq!(snapshot.width, 4);
        assert_eq!(snapshot.height, 4);
        let red_values = snapshot
            .rgba
            .chunks_exact(4)
            .map(|pixel| pixel[0])
            .collect::<Vec<_>>();
        assert_eq!(
            red_values,
            vec![10, 11, 12, 13, 18, 19, 20, 21, 26, 27, 28, 29, 34, 35, 36, 37]
        );
    }

    #[test]
    fn snapshot_paths_skip_names_that_already_exist() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "wavecraft-snapshot-paths-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();

        assert_eq!(
            available_snapshot_path(&directory),
            Some(directory.join("wavecraft-snapshot.png"))
        );
        std::fs::write(directory.join("wavecraft-snapshot.png"), b"x").unwrap();
        assert_eq!(
            available_snapshot_path(&directory),
            Some(directory.join("wavecraft-snapshot-2.png"))
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn snapshot_crop_rejects_invalid_bounds_and_scale() {
        let screenshot = egui::ColorImage::new([4, 3], Color32::BLACK);

        assert!(crop_snapshot_image(
            &screenshot,
            Rect::from_min_max(Pos2::new(5.0, 1.0), Pos2::new(6.0, 2.0)),
            1.0,
        )
        .is_err());
        assert!(crop_snapshot_image(
            &screenshot,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(2.0, 2.0)),
            0.0,
        )
        .is_err());
    }
}
