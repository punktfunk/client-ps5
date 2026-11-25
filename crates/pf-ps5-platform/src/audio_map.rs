//! The stream's channels into `AudioOut`'s. The wire orders eight channels FL FR FC LFE BL BR
//! SL SR, and the N-channel order is its first N. The console's eight-channel port takes FL FR
//! FC LFE SL SR BL BR (shadPS4 `sdl_audio_out.cpp`). Pure, so it tests anywhere.

/// The port width that plays `channels`: stereo as it is, 5.1 and 7.1 on the eight-channel
/// port. `None` for a layout the console has no port for.
pub fn port_channels(channels: usize) -> Option<usize> {
    match channels {
        2 => Some(2),
        6 | 8 => Some(8),
        _ => None,
    }
}

/// The port slot each wire channel plays from. A 5.1 stream's rear pair lands on the port's
/// surround pair, which is what a 5.1 speaker set has; 7.1 trades the two pairs' places.
fn slots(channels: usize) -> &'static [usize] {
    match channels {
        8 => &[0, 1, 2, 3, 6, 7, 4, 5],
        6 => &[0, 1, 2, 3, 4, 5],
        _ => &[0, 1],
    }
}

/// Interleaved `src` frames of `channels` into S16 frames of `width`; slots nothing feeds
/// stay silent.
pub fn interleave(src: &[f32], channels: usize, dst: &mut [i16], width: usize) {
    dst.fill(0);
    let slots = slots(channels);
    for (frame, out) in src.chunks_exact(channels).zip(dst.chunks_exact_mut(width)) {
        for (sample, &slot) in frame.iter().zip(slots) {
            out[slot] = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame whose sample at wire channel `n` is `(n + 1) / 10`.
    fn frame(channels: usize) -> Vec<f32> {
        (1..=channels).map(|n| n as f32 / 10.0).collect()
    }

    fn played(channels: usize) -> Vec<i16> {
        let width = port_channels(channels).unwrap();
        let mut dst = vec![-1; width];
        interleave(&frame(channels), channels, &mut dst, width);
        dst.iter().map(|s| (f32::from(*s) / 3276.7).round() as i16).collect()
    }

    #[test]
    fn seven_one_trades_the_rear_and_side_pairs() {
        assert_eq!(played(8), [1, 2, 3, 4, 7, 8, 5, 6]);
    }

    #[test]
    fn five_one_plays_on_the_surround_pair_and_leaves_the_last_silent() {
        assert_eq!(played(6), [1, 2, 3, 4, 5, 6, 0, 0]);
    }

    #[test]
    fn stereo_is_itself_and_other_widths_have_no_port() {
        assert_eq!(played(2), [1, 2]);
        assert_eq!(port_channels(4), None);
    }
}
