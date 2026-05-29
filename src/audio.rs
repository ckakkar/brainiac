use anyhow::{bail, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

use crate::events::AppEvent;

// Commands sent from the TUI to the mic thread.
enum MicCmd {
    Start {
        audio_tx: UnboundedSender<Vec<i16>>,
        level_tx: UnboundedSender<AppEvent>,
    },
    Stop,
}

/// Mic handle. Owns the command channel to the dedicated mic OS thread.
/// The cpal::Stream stays on the thread that created it (required on macOS CoreAudio).
/// This struct is Send because it only holds a std::sync::mpsc::SyncSender.
pub struct Microphone {
    cmd_tx: std::sync::mpsc::SyncSender<MicCmd>,
    pub sample_rate: u32,
}

impl Microphone {
    pub fn spawn() -> Result<Self> {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::sync_channel::<MicCmd>(4);
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<Result<u32>>(1);

        std::thread::Builder::new()
            .name("brainiac-mic".into())
            .spawn(move || {
                let result = (|| {
                    let host = cpal::default_host();
                    let device = host
                        .default_input_device()
                        .ok_or_else(|| anyhow::anyhow!("No default input device found"))?;

                    let supported = device.default_input_config()?;
                    let sample_rate = supported.sample_rate().0;
                    let channels = supported.channels() as usize;
                    let fmt = supported.sample_format();
                    let stream_cfg = supported.config();

                    // Shared slot: only Some when actively recording.
                    let slot: Arc<Mutex<Option<(UnboundedSender<Vec<i16>>, UnboundedSender<AppEvent>)>>> =
                        Arc::new(Mutex::new(None));
                    let slot_cb = Arc::clone(&slot);

                    let build_stream = |conv: Box<dyn Fn(&[f32]) -> Vec<i16> + Send + 'static>| {
                        device.build_input_stream(
                            &stream_cfg,
                            move |data: &[f32], _| {
                                if let Ok(guard) = slot_cb.lock() {
                                    if let Some((ref audio_tx, ref level_tx)) = *guard {
                                        let level = data
                                            .iter()
                                            .map(|&s| s.abs())
                                            .fold(0.0_f32, f32::max);
                                        let _ = level_tx.send(AppEvent::AudioLevel(level));
                                        let _ = audio_tx.send(conv(data));
                                    }
                                }
                            },
                            |err| eprintln!("[mic] stream error: {err}"),
                            None,
                        )
                    };

                    // We always convert to f32 internally and then to i16.
                    let stream = match fmt {
                        cpal::SampleFormat::F32 => {
                            let ch = channels;
                            build_stream(Box::new(move |data: &[f32]| f32_to_i16_mono(data, ch)))?
                        }
                        cpal::SampleFormat::I16 => {
                            let ch = channels;
                            // cpal doesn't support building an f32 callback from i16 stream directly;
                            // re-build with native i16 config.
                            let slot_cb2 = Arc::clone(&slot);
                            device.build_input_stream(
                                &stream_cfg,
                                move |data: &[i16], _| {
                                    if let Ok(guard) = slot_cb2.lock() {
                                        if let Some((ref audio_tx, ref level_tx)) = *guard {
                                            let mono = i16_to_mono(data, ch);
                                            let level = mono
                                                .iter()
                                                .map(|&s| (s as f32 / i16::MAX as f32).abs())
                                                .fold(0.0_f32, f32::max);
                                            let _ = level_tx.send(AppEvent::AudioLevel(level));
                                            let _ = audio_tx.send(mono);
                                        }
                                    }
                                },
                                |err| eprintln!("[mic] stream error: {err}"),
                                None,
                            )?
                        }
                        _ => bail!("Unsupported sample format: {:?}", fmt),
                    };

                    stream.pause()?;
                    let _ = init_tx.send(Ok(sample_rate));

                    // Command loop — stream is kept alive here.
                    while let Ok(cmd) = cmd_rx.recv() {
                        match cmd {
                            MicCmd::Start { audio_tx, level_tx } => {
                                *slot.lock().unwrap() = Some((audio_tx, level_tx));
                                if let Err(e) = stream.play() {
                                    eprintln!("[mic] play error: {e}");
                                }
                            }
                            MicCmd::Stop => {
                                // Drop senders → audio channel closes → STT task exits.
                                *slot.lock().unwrap() = None;
                                if let Err(e) = stream.pause() {
                                    eprintln!("[mic] pause error: {e}");
                                }
                            }
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                })();

                if let Err(e) = result {
                    let _ = init_tx.send(Err(e));
                }
            })?;

        let sample_rate = init_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("Mic thread died before init"))??;

        Ok(Self { cmd_tx, sample_rate })
    }

    /// Start recording. Returns a receiver for raw PCM i16 mono chunks.
    /// Dropping the sender (via stop_listening) closes the channel, which signals
    /// the STT task to finalize.
    pub fn start_listening(
        &self,
        event_tx: UnboundedSender<AppEvent>,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<Vec<i16>>> {
        let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel();
        self.cmd_tx
            .send(MicCmd::Start {
                audio_tx,
                level_tx: event_tx,
            })
            .map_err(|_| anyhow::anyhow!("Mic thread disconnected"))?;
        Ok(audio_rx)
    }

    /// Stop recording. Drops the audio sender → STT task sees channel closed.
    pub fn stop_listening(&self) {
        let _ = self.cmd_tx.send(MicCmd::Stop);
    }
}

fn f32_to_i16_mono(data: &[f32], channels: usize) -> Vec<i16> {
    if channels == 1 {
        data.iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect()
    } else {
        data.chunks(channels)
            .map(|frame| {
                let avg = frame.iter().sum::<f32>() / channels as f32;
                (avg.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
            })
            .collect()
    }
}

fn i16_to_mono(data: &[i16], channels: usize) -> Vec<i16> {
    if channels == 1 {
        data.to_vec()
    } else {
        data.chunks(channels)
            .map(|frame| {
                (frame.iter().map(|&s| s as i32).sum::<i32>() / channels as i32) as i16
            })
            .collect()
    }
}
