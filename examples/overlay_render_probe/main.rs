//! Offscreen `AppKit` drawing probe. Does not start a daemon or order any windows.
#![allow(dead_code)]

mod platform {
    pub type WinID = i32;
    pub type WorkspaceId = u64;
}

mod manager {
    pub fn move_owned_window_to_space(_: i32, _: u64) -> Result<(), &'static str> {
        Err("the offscreen probe must not bind native windows")
    }
}

mod overlay {
    include!("../../src/overlay.rs");

    fn draw_damage(
        view: &DecorationView,
        context: &NSGraphicsContext,
        damage: &[NSRect],
        scale: f64,
    ) {
        use objc2_app_kit::NSAffineTransformNSAppKitAdditions;
        use objc2_foundation::NSAffineTransform;

        if damage.is_empty() {
            return;
        }
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(context));
        context.saveGraphicsState();
        let transform = NSAffineTransform::transform();
        transform.translateXBy_yBy(0.0, view.bounds().size.height * scale);
        transform.scaleXBy_yBy(scale, -scale);
        transform.concat();
        let clip = NSBezierPath::bezierPath();
        for rect in damage {
            clip.appendBezierPathWithRect(*rect);
        }
        clip.addClip();
        // Use fixed view coordinates. An unattached view's displayRect method
        // relocates each subrectangle in the bitmap, unlike a window backing.
        view.draw_rect(objc2::sel!(drawRect:), view.bounds());
        context.restoreGraphicsState();
        NSGraphicsContext::restoreGraphicsState_class();
    }

    pub fn run() {
        use std::time::Instant;

        use objc2_app_kit::{NSApplication, NSBitmapImageRep, NSDeviceRGBColorSpace};

        let mtm = MainThreadMarker::new().expect("probe runs on the main thread");
        let _app = NSApplication::sharedApplication(mtm);
        let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(2940.0, 1912.0));
        let bitmap = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(), std::ptr::null_mut(), 2940, 1912,
                8, 4, true, false, NSDeviceRGBColorSpace, 0, 0,
            )
        }.expect("RGBA bitmap");
        let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)
            .expect("bitmap drawing context");
        let full_redraw = std::env::args().any(|arg| arg == "--full-redraw");
        println!("full_redraw={full_redraw}");

        for dim_opacity in [0.0, 0.2] {
            for moving in [false, true] {
                let mut state = DecorationDrawState {
                    style: DecorationStyle {
                        dim_opacity,
                        dim_color: (0.0, 0.0, 0.0),
                        cutout_radius: 12.0,
                        border: Some(BorderParams {
                            color: (0.3, 0.6, 1.0),
                            opacity: 1.0,
                            width: 4.0,
                            radius: 12.0,
                        }),
                    },
                    cutout: None,
                    border_rect: None,
                };
                let view = DecorationView::new(mtm, bounds, &state);
                draw_damage(&view, &context, &[bounds], 1.0);
                let mut samples = Vec::new();
                let mut draws = 0;
                for frame in 0..300 {
                    let x = if moving {
                        f64::from(frame % 120) * 8.0
                    } else {
                        100.0
                    };
                    let rect = NSRect::new(NSPoint::new(x, 40.0), NSSize::new(1400.0, 1800.0));
                    state.border_rect = Some(rect);
                    state.cutout = (dim_opacity != 0.0).then_some(rect);
                    let start = Instant::now();
                    let damage = view.update(&state);
                    let damage = if full_redraw { vec![bounds] } else { damage };
                    if !damage.is_empty() {
                        draw_damage(&view, &context, &damage, 1.0);
                        view.setNeedsDisplay(false);
                        draws += 1;
                    }
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                }
                samples.sort_by(f64::total_cmp);
                let byte_count =
                    usize::try_from(bitmap.bytesPerRow()).expect("positive row size") * 1912;
                // The bitmap owns a contiguous, initialized RGBA buffer of this size.
                let pixels = unsafe { std::slice::from_raw_parts(bitmap.bitmapData(), byte_count) };
                assert!(
                    pixels.iter().any(|byte| *byte != 0),
                    "probe must actually draw pixels"
                );
                println!(
                    "dim={dim_opacity} moving={moving} updates=300 draws={draws} median_ms={:.3} p95_ms={:.3} max_ms={:.3}",
                    samples[150], samples[285], samples[299]
                );
            }
        }
        for scale in [1, 2] {
            verify_incremental_pixels(mtm, scale);
        }
    }

    fn verify_incremental_pixels(mtm: MainThreadMarker, scale: i32) {
        use objc2_app_kit::{NSBitmapImageRep, NSDeviceRGBColorSpace};

        let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(800.0, 600.0));
        let make_bitmap = || unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(), std::ptr::null_mut(), 800 * scale as isize, 600 * scale as isize,
                8, 4, true, false, NSDeviceRGBColorSpace, 0, 0,
            ).expect("RGBA bitmap")
        };
        let actual = make_bitmap();
        let reference = make_bitmap();
        let actual_context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&actual).unwrap();
        let reference_context =
            NSGraphicsContext::graphicsContextWithBitmapImageRep(&reference).unwrap();
        let frames = [
            Some((10.0, 20.0, 400.0, 500.0)),
            Some((18.25, 24.5, 400.0, 500.0)),
            Some((18.25, 24.5, 400.0, 500.0)),
            Some((30.0, 40.0, 350.0, 450.0)),
            Some((600.0, 500.0, 400.0, 500.0)),
            Some((-100.5, -20.25, 450.0, 400.0)),
            Some((100.0, 100.0, 10.0, 8.0)),
            None,
            Some((100.0, 100.0, 600.0, 400.0)),
            Some((1000.0, 1000.0, 600.0, 400.0)),
            Some((100.0, 100.0, 600.0, 400.0)),
        ];
        let mut verified = 0;
        for dim_opacity in [0.0, 0.2] {
            let mut state = DecorationDrawState {
                style: DecorationStyle {
                    dim_opacity,
                    dim_color: (0.1, 0.2, 0.3),
                    cutout_radius: 12.0,
                    border: Some(BorderParams {
                        color: (0.3, 0.6, 1.0),
                        opacity: 0.7,
                        width: 4.0,
                        radius: 12.0,
                    }),
                },
                cutout: None,
                border_rect: None,
            };
            let view = DecorationView::new(mtm, bounds, &state);
            draw_damage(&view, &actual_context, &[bounds], f64::from(scale));
            for (index, frame) in frames.into_iter().enumerate() {
                state.border_rect =
                    frame.map(|(x, y, w, h)| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)));
                state.cutout = (dim_opacity != 0.0).then_some(state.border_rect).flatten();
                // Also exercise style changes, which invalidate the whole backing.
                if index == frames.len() - 1 {
                    state.style.dim_color = (0.3, 0.1, 0.2);
                    state.style.border.as_mut().unwrap().width = 10.0;
                }
                draw_damage(
                    &view,
                    &actual_context,
                    &view.update(&state),
                    f64::from(scale),
                );
                draw_damage(&view, &reference_context, &[bounds], f64::from(scale));
                let byte_count = usize::try_from(actual.bytesPerRow()).unwrap()
                    * 600
                    * usize::try_from(scale).unwrap();
                // Both bitmaps own initialized, equally sized contiguous RGBA buffers.
                let (actual_pixels, reference_pixels) = unsafe {
                    (
                        std::slice::from_raw_parts(actual.bitmapData(), byte_count),
                        std::slice::from_raw_parts(reference.bitmapData(), byte_count),
                    )
                };
                let mismatches = actual_pixels
                    .iter()
                    .zip(reference_pixels)
                    .filter(|(a, b)| a != b)
                    .count();
                if mismatches != 0 {
                    for (name, bitmap) in [("actual", &actual), ("reference", &reference)] {
                        let data = unsafe {
                            bitmap.representationUsingType_properties(
                                objc2_app_kit::NSBitmapImageFileType::PNG,
                                &NSDictionary::new(),
                            )
                        }
                        .expect("PNG representation");
                        std::fs::write(format!("/tmp/spool-overlay-{name}.png"), data.to_vec())
                            .unwrap();
                    }
                }
                assert_eq!(
                    mismatches, 0,
                    "incremental/full pixel mismatch: scale={scale} dim={dim_opacity} frame={index}"
                );
                verified += 1;
            }
        }
        println!("scale={scale} incremental_vs_full_frames={verified} pixel_mismatches=0");
    }
}

fn main() {
    objc2::rc::autoreleasepool(|_| overlay::run());
}
