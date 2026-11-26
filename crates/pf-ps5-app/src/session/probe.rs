//! The network speed test's handshake-only connection.
//!
//! It loads no video backend and spawns no pump — see the `connect` module for the streaming
//! path. It blocks, so its caller runs it on a worker thread.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use punktfunk_core::client::{NativeClient, ProbeOutcome};
use punktfunk_core::config::Mode;
use punktfunk_core::{quic, PunktfunkError};

/// Opens a handshake-only session: no video backend loads, no pump thread spawns, nothing is
/// presented. A throwaway 720p mode, whole-AU delivery, no cursor, no HDR metadata, the host's
/// own device label.
///
/// `video_caps` matches a real session's (AES-GCM, which the console's CPU accelerates):
/// `punktfunk-core` counts delivered bytes after decrypt, so the probe measures the cipher a
/// stream uses.
fn handshake_only(
    host: &str,
    port: u16,
    bitrate_kbps: u32,
    video_codecs: u8,
    pin: Option<[u8; 32]>,
    identity: (String, String),
    timeout: Duration,
) -> punktfunk_core::Result<NativeClient> {
    // The negotiated mode is irrelevant here (immediately dropped); a small 720p request keeps
    // the host from doing needless 4K/HEVC setup for a connection we close at once.
    let mode = Mode {
        width: 1280,
        height: 720,
        refresh_hz: 60,
        refresh_mhz: 0,
    };
    NativeClient::connect(punktfunk_core::client::ConnectParams {
        bitrate_kbps,
        video_caps: 0,
        video_codecs,
        pin,
        identity: Some(identity),
        ..punktfunk_core::client::ConnectParams::new(host, port, mode, timeout)
    })
}

/// What the host is asked to burst during a speed test, and for how long.
///
/// Chosen against what the answer is used for: the recommendation is 70 % of measured, for
/// HEVC and H.264 rates the console decodes far below this. A burst far past capacity mostly
/// measures the access point's drop policy and starves the host's end-of-burst report, which
/// shares the saturated path. A wired console may justify more once a bundle shows its ceiling.
const PROBE_TARGET_KBPS: u32 = 320_000;
/// A pinned (non-zero) session rate for the probe connect — see the call site: its only
/// job is to keep `bitrate_kbps == 0` from arming core's own capacity probe against the
/// single shared `ProbeState`. Nothing decodes here, so the value itself is immaterial.
const PROBE_SESSION_BITRATE_KBPS: u32 = 20_000;
/// Below this many delivered bytes, a missing host report is a failure rather than
/// something to salvage — 1 MB over a 3 s burst is ~2.7 Mbps, far under anything worth
/// recommending a bitrate from.
const SALVAGE_MIN_BYTES: u64 = 1024 * 1024;
const PROBE_DURATION_MS: u32 = 3_000;
/// How long to wait for the data plane to prove itself live (first completed video frame)
/// before bursting: a new UDP flow can be held by an access point for tens of seconds, and a
/// burst into that hold measures the hold. Past the cap the burst goes ahead anyway.
const PROBE_WARMUP_CAP: Duration = Duration::from_secs(35);
/// How long to keep polling for the host's end-of-burst report after the burst should
/// have finished before giving up. Generous: the report shares a link the burst has just
/// been hammering, so its first delivery attempt can well be lost and need a retransmit.
const PROBE_REPORT_GRACE: Duration = Duration::from_secs(12);

/// A finished speed test, and whether the host confirmed the figures.
pub struct SpeedProbeResult {
    pub outcome: ProbeOutcome,
    /// `true` when the host's end-of-burst report arrived. `false` means it never did and
    /// the throughput was derived from what this client actually received over the burst
    /// window it asked for — a real measurement, but with no host-side cross-check, so no
    /// loss figure and a conservative reading if the burst was cut short.
    pub confirmed: bool,
}

/// Runs one network speed test against `host` and returns the host's final measurement.
///
/// Uses [`NativeClient`] directly rather than `session::connect`:
/// no video backend is loaded and no pump thread is spawned, so the punch-through plane
/// is never touched — the host builds a virtual output, but nothing is decoded or
/// presented. Blocks; run it on a worker thread.
///
/// `progress` is called with each partial poll so the UI can show the figure climbing.
pub fn run_speed_probe(
    host: &str,
    port: u16,
    identity: (String, String),
    pin: Option<[u8; 32]>,
    timeout: Duration,
    mut progress: impl FnMut(ProbeOutcome),
) -> Result<SpeedProbeResult> {
    let client = handshake_only(
        host,
        port,
        // Not 0: zero arms core's own startup capacity probe, which shares the one
        // `ProbeState` slot with ours. A pinned rate disarms it; nothing decodes here.
        PROBE_SESSION_BITRATE_KBPS,
        quic::CODEC_HEVC | quic::CODEC_H264,
        pin,
        identity,
        timeout,
    )
    .context("speed test connect")?;

    // The negotiated session, logged before the burst: if a measurement comes back
    // empty, this line is what says whether the connection itself was sane.
    tracing::info!(
        "speed test connected: codec={} audio_ch={} resolved_bitrate_kbps={}",
        client.codec,
        client.audio_channels,
        client.resolved_bitrate_kbps,
    );

    // Don't burst into a dead plane — see PROBE_WARMUP_CAP. `next_frame` drains the session's
    // decode-less video into the void; the first completed frame is the "plane is live" edge.
    let warmup = Instant::now();
    let mut warmed = false;
    while warmup.elapsed() < PROBE_WARMUP_CAP {
        match client.next_frame(Duration::from_millis(250)) {
            Ok(_) => {
                warmed = true;
                break;
            }
            Err(PunktfunkError::NoFrame) => {}
            // Anything else (a closed session, say) answers at once and every time: polling on
            // would spin a core until the cap. `request_probe` below reports the failure.
            Err(e) => {
                tracing::warn!("speed test: data plane ended during warmup: {e:#}");
                break;
            }
        }
    }
    tracing::info!(
        "speed test: data plane {} after {} ms",
        if warmed {
            "live"
        } else {
            "still silent (proceeding anyway)"
        },
        warmup.elapsed().as_millis(),
    );

    let requested = client
        .request_probe(PROBE_TARGET_KBPS, PROBE_DURATION_MS)
        .context("request_probe");
    if requested.is_err() {
        // Same deliberate teardown as every other way out of here.
        client.disconnect_quit();
    }
    requested?;
    // Flip the UI from "Connecting…" to "Measuring…" the moment the burst is requested —
    // with the warmup above, the first 250 ms poll is no longer the earliest signal.
    progress(client.probe_result());

    let deadline = Instant::now() + Duration::from_millis(u64::from(PROBE_DURATION_MS)) + PROBE_REPORT_GRACE;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let outcome = client.probe_result();
        if outcome.done {
            // Let the last in-flight UDP shards land before tearing the connection
            // down, so the delivered-bytes figure isn't cut short by our own exit.
            std::thread::sleep(Duration::from_millis(400));
            let final_outcome = client.probe_result();
            // Both sides: `host_bytes == 0` is the host never sending; `host_bytes > 0` with
            // `recv_bytes == 0` is a path or decrypt problem (core counts after decrypt).
            tracing::info!(
                "speed test result: recv_bytes={} recv_packets={} host_bytes={} host_packets={} \
                 elapsed_ms={} throughput_kbps={} loss_pct={:.2} host_drop_pct={:.2} \
                 wire_packets_sent={} send_dropped={}",
                final_outcome.recv_bytes,
                final_outcome.recv_packets,
                final_outcome.host_bytes,
                final_outcome.host_packets,
                final_outcome.elapsed_ms,
                final_outcome.throughput_kbps,
                final_outcome.loss_pct,
                final_outcome.host_drop_pct,
                final_outcome.wire_packets_sent,
                final_outcome.send_dropped,
            );
            client.disconnect_quit();
            return Ok(SpeedProbeResult {
                outcome: final_outcome,
                confirmed: true,
            });
        }
        progress(outcome);
        if Instant::now() > deadline {
            // No report, but `recv_bytes` counts live during the burst: divide it by the
            // window asked for, which the host honours. Only loss needs the host's count.
            let mut salvaged = client.probe_result();
            client.disconnect_quit();
            if salvaged.recv_bytes >= SALVAGE_MIN_BYTES {
                salvaged.elapsed_ms = PROBE_DURATION_MS;
                salvaged.throughput_kbps =
                    (salvaged.recv_bytes.saturating_mul(8) / u64::from(PROBE_DURATION_MS)) as u32;
                tracing::warn!(
                    "speed test: no host report; salvaged from {} received bytes over the {} ms \
                     burst window -> {} kbps (unconfirmed)",
                    salvaged.recv_bytes,
                    PROBE_DURATION_MS,
                    salvaged.throughput_kbps,
                );
                return Ok(SpeedProbeResult {
                    outcome: salvaged,
                    confirmed: false,
                });
            }
            anyhow::bail!(
                "the host never sent its result, and almost nothing arrived. The test burst can \
                 saturate the link the result has to come back over — try again, or use a wired \
                 connection."
            );
        }
    }
}
