//! `bench`: times the codec workloads griasdi depends on with the
//! `gst-launch-1.0` of a finished prefix, so that a change to the recipe's
//! compiler flags shows up as numbers. Every platform job runs it after
//! `dist`; a workload that fails fails the job, which makes it the smoke test
//! of the codecs that ship.
//!
//! Each workload runs once untimed (the first run also scans the prefix into
//! a fresh registry), then three times timed; the median is reported.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, ensure};

use crate::build;
use crate::plan::{Os, host_triple};
use crate::tools::download;

/// A media file the decode workloads read, pinned by its SHA-256. These are
/// not in `pins.rs`: they do not change the binaries.
struct Media {
    url: &'static str,
    sha256: &'static str,
}

/// The Sintel trailer (CC-BY 3.0, Blender Foundation): H.264 High,
/// 1920x1080, 24 fps, 1253 frames, plus AAC.
const H264: Media = Media {
    url: "https://download.blender.org/durian/trailer/sintel_trailer-1080p.mp4",
    sha256: "34bbd52a4b89fdf63c8ace50b268da26653a59508288100cd3c23de276db7931",
};

/// Big Buck Bunny (CC-BY 3.0, Blender Foundation), 10 s: VP9 profile 0,
/// 1920x1080, 30 fps, 300 frames, no audio.
const VP9: Media = Media {
    url: "https://test-videos.co.uk/vids/bigbuckbunny/webm/vp9/1080/Big_Buck_Bunny_1080_10s_5MB.webm",
    sha256: "3011a3d7dc742d6c0f37194ba8273e6b09b90fe768d5f11386ff140bc6745d52",
};

/// One timed pipeline: its gst-launch tokens (`{h264}` and `{vp9}` stand for
/// the media files) and how many `unit`s one run processes.
struct Workload {
    name: &'static str,
    args: &'static [&'static str],
    count: u32,
    unit: &'static str,
}

/// No Opus workload: GStreamer's `opusenc` uses its own libopus, not the one
/// griasdi's voice links; griasdi's survey measures that one.
const WORKLOADS: [Workload; 5] = [
    Workload {
        name: "h264-decode",
        args: &[
            "filesrc",
            "location={h264}",
            "!",
            "qtdemux",
            "!",
            "h264parse",
            "!",
            "avdec_h264",
            "max-threads=1",
            "!",
            "fakesink",
            "sync=false",
        ],
        count: 1253,
        unit: "frames",
    },
    Workload {
        name: "vp9-decode",
        args: &[
            "filesrc",
            "location={vp9}",
            "!",
            "matroskademux",
            "!",
            "vp9parse",
            "!",
            "avdec_vp9",
            "max-threads=1",
            "!",
            "fakesink",
            "sync=false",
        ],
        count: 300,
        unit: "frames",
    },
    Workload {
        name: "av1-encode",
        args: &[
            "videotestsrc",
            "num-buffers=300",
            "pattern=smpte",
            "horizontal-speed=4",
            "!",
            "video/x-raw,format=I420,width=1280,height=720,framerate=30/1",
            "!",
            "svtav1enc",
            "preset=10",
            "target-bitrate=2500",
            "intra-period-length=299",
            "!",
            "fakesink",
            "sync=false",
        ],
        count: 300,
        unit: "frames",
    },
    Workload {
        name: "videoconvertscale",
        args: &[
            "videotestsrc",
            "num-buffers=600",
            "pattern=smpte",
            "horizontal-speed=4",
            "!",
            "video/x-raw,format=BGRA,width=1920,height=1080,framerate=60/1",
            "!",
            "videoconvertscale",
            "!",
            "video/x-raw,format=I420,width=1280,height=720",
            "!",
            "fakesink",
            "sync=false",
        ],
        count: 600,
        unit: "frames",
    },
    Workload {
        name: "audioresample",
        args: &[
            "audiotestsrc",
            "num-buffers=20000",
            "samplesperbuffer=1024",
            "wave=pink-noise",
            "!",
            "audio/x-raw,format=F32LE,rate=44100,channels=2",
            "!",
            "audioresample",
            "!",
            "audio/x-raw,rate=48000",
            "!",
            "fakesink",
            "sync=false",
        ],
        count: 20000,
        unit: "buffers",
    },
];

/// Timed runs per workload, after one untimed run.
const RUNS: usize = 3;

/// Runs every workload with the prefix of `target` (the host when `None`)
/// and prints the results to stdout.
pub fn bench(target: Option<&str>) -> Result<()> {
    let layout = build::existing(target)?;
    let triple = &layout.triple;
    ensure!(
        *triple == host_triple()?,
        "bench runs the programs of the prefix; {triple} does not run on this host"
    );
    let os = Os::of(triple);
    let exe = if os == Os::Windows { ".exe" } else { "" };
    let launch = layout.prefix.join(format!("bin/gst-launch-1.0{exe}"));
    ensure!(launch.is_file(), "{} is missing", launch.display());

    let h264 = download(&layout.tools(), H264.url, H264.sha256)?;
    let vp9 = download(&layout.tools(), VP9.url, VP9.sha256)?;

    let state = layout.base.join(format!("bench-{triple}"));
    std::fs::create_dir_all(&state)?;
    let registry = state.join("registry.bin");
    if registry.exists() {
        std::fs::remove_file(&registry)?;
    }
    let lib = layout.prefix.join("lib");
    let library_path = match os {
        Os::Linux => ("LD_LIBRARY_PATH", lib.clone().into_os_string()),
        Os::Macos => ("DYLD_LIBRARY_PATH", lib.clone().into_os_string()),
        Os::Windows => {
            let mut path = vec![layout.prefix.join("bin")];
            path.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            ("PATH", std::env::join_paths(path)?)
        }
    };

    let recipe = std::fs::read_to_string(layout.prefix.join(".recipe"))?;
    println!(
        "bench {triple} recipe={} cpu=\"{}\" threads={}",
        recipe.trim().get(..12).unwrap_or(recipe.trim()),
        cpu_model(os),
        std::thread::available_parallelism().map_or(0, |n| n.get())
    );
    for workload in &WORKLOADS {
        let args: Vec<String> = workload
            .args
            .iter()
            .map(|arg| token(arg, &h264, &vp9))
            .collect();
        let mut seconds = Vec::with_capacity(RUNS);
        for run in 0..=RUNS {
            eprintln!("bench {} run {run} of {RUNS}", workload.name);
            let mut command = Command::new(&launch);
            command
                .arg("-q")
                .args(&args)
                .env("GST_REGISTRY_1_0", &registry)
                .env("GST_PLUGIN_SYSTEM_PATH_1_0", lib.join("gstreamer-1.0"))
                .env_remove("GST_PLUGIN_PATH_1_0")
                .env_remove("GST_PLUGIN_PATH")
                .env("GST_REGISTRY_FORK", "no")
                .env(library_path.0, &library_path.1);
            let start = Instant::now();
            let output = command
                .output()
                .with_context(|| format!("failed to run {}", launch.display()))?;
            let elapsed = start.elapsed().as_secs_f64();
            ensure!(
                output.status.success(),
                "bench {} failed with {}:\n{}{}",
                workload.name,
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if run > 0 {
                seconds.push(elapsed);
            }
        }
        let mut sorted = seconds.clone();
        sorted.sort_by(f64::total_cmp);
        let median = sorted[RUNS / 2];
        println!(
            "{:<18} median {median:>7.3} s  {:>9.1} {}/s  runs {:.3},{:.3},{:.3}",
            workload.name,
            f64::from(workload.count) / median,
            workload.unit,
            seconds[0],
            seconds[1],
            seconds[2]
        );
    }
    Ok(())
}

/// `arg` with `{h264}` and `{vp9}` replaced by the media files, with `/` as
/// the separator: gst-launch reads `\` as an escape.
fn token(arg: &str, h264: &Path, vp9: &Path) -> String {
    let path = |p: &Path| p.display().to_string().replace('\\', "/");
    arg.replace("{h264}", &path(h264))
        .replace("{vp9}", &path(vp9))
}

/// The CPU's model name, or `unknown`.
fn cpu_model(os: Os) -> String {
    let model = match os {
        Os::Linux => std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|c| {
            c.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_owned())
        }),
        Os::Macos => Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()),
        Os::Windows => std::env::var("PROCESSOR_IDENTIFIER").ok(),
    };
    model
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_media_path_becomes_a_forward_slash_token() {
        assert_eq!(
            token(
                "location={h264}",
                Path::new(r"C:\t\sintel.mp4"),
                Path::new("v.webm")
            ),
            "location=C:/t/sintel.mp4"
        );
    }
}
