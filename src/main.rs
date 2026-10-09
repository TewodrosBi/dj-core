use rtrb::Consumer;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::thread;
use std::time::Duration;

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct StereoStemFrame {
    pub drums: [f32; 2],
    pub bass: [f32; 2],
    pub instruments: [f32; 2],
    pub vocals: [f32; 2],
}

#[derive(Clone, Copy, Debug)]
pub struct StemsGainControl {
    pub drums_gain: f32,
    pub bass_gain: f32,
    pub instruments_gain: f32,
    pub vocals_gain: f32,
}

impl Default for StemsGainControl {
    fn default() -> Self {
        Self {
            drums_gain: 1.0,
            bass_gain: 1.0,
            instruments_gain: 1.0,
            vocals_gain: 1.0,
        }
    }
}

impl StemsGainControl {
    pub fn mix(&self, frame: &StereoStemFrame) -> [f32; 2] {
        let l = frame.drums[0] * self.drums_gain
              + frame.bass[0] * self.bass_gain
              + frame.instruments[0] * self.instruments_gain
              + frame.vocals[0] * self.vocals_gain;

        let r = frame.drums[1] * self.drums_gain
              + frame.bass[1] * self.bass_gain
              + frame.instruments[1] * self.instruments_gain
              + frame.vocals[1] * self.vocals_gain;

        [l, r]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ControlCommand {
    SetDrumsGain(f32),
    SetBassGain(f32),
    SetInstrumentsGain(f32),
    SetVocalsGain(f32),
}

pub struct AudioPipeline {
    audio_rx: Consumer<StereoStemFrame>,
    command_rx: Consumer<ControlCommand>,
    gain_control: StemsGainControl,
}

impl AudioPipeline {
    pub fn new(audio_rx: Consumer<StereoStemFrame>, command_rx: Consumer<ControlCommand>) -> Self {
        Self {
            audio_rx,
            command_rx,
            gain_control: StemsGainControl::default(),
        }
    }

    fn apply_commands(&mut self) {
        while let Ok(command) = self.command_rx.pop() {
            match command {
                ControlCommand::SetDrumsGain(g) => self.gain_control.drums_gain = g,
                ControlCommand::SetBassGain(g) => self.gain_control.bass_gain = g,
                ControlCommand::SetInstrumentsGain(g) => self.gain_control.instruments_gain = g,
                ControlCommand::SetVocalsGain(g) => self.gain_control.vocals_gain = g,
            }
        }
    }

    /// Process output buffer, which is expected to be an interleaved buffer [L, R, L, R, ...]
    pub fn process_output_f32(&mut self, output: &mut [f32], channels: usize) {
        self.apply_commands();
        let frames = output.chunks_exact_mut(channels);

        for frame_out in frames {
            let mixed = if let Ok(stem_frame) = self.audio_rx.pop() {
                self.gain_control.mix(&stem_frame)
            } else {
                [0.0, 0.0]
            };

            if channels >= 2 {
                frame_out[0] = mixed[0];
                frame_out[1] = mixed[1];
            } else if channels == 1 {
                frame_out[0] = (mixed[0] + mixed[1]) * 0.5;
            }
            for ch in frame_out.iter_mut().skip(2) { *ch = 0.0; }
        }
    }

    pub fn process_output_i16(&mut self, output: &mut [i16], channels: usize) {
        self.apply_commands();
        let frames = output.chunks_exact_mut(channels);

        for frame_out in frames {
            let mixed = if let Ok(stem_frame) = self.audio_rx.pop() {
                self.gain_control.mix(&stem_frame)
            } else {
                [0.0, 0.0]
            };

            let mix_l = mixed[0].clamp(-1.0, 1.0);
            let mix_r = mixed[1].clamp(-1.0, 1.0);

            if channels >= 2 {
                frame_out[0] = (mix_l * i16::MAX as f32) as i16;
                frame_out[1] = (mix_r * i16::MAX as f32) as i16;
            } else if channels == 1 {
                frame_out[0] = (((mix_l + mix_r) * 0.5) * i16::MAX as f32) as i16;
            }
            for ch in frame_out.iter_mut().skip(2) { *ch = 0; }
        }
    }

    pub fn process_output_u16(&mut self, output: &mut [u16], channels: usize) {
        self.apply_commands();
        let frames = output.chunks_exact_mut(channels);

        for frame_out in frames {
            let mixed = if let Ok(stem_frame) = self.audio_rx.pop() {
                self.gain_control.mix(&stem_frame)
            } else {
                [0.0, 0.0]
            };

            let mix_l = mixed[0].clamp(-1.0, 1.0);
            let mix_r = mixed[1].clamp(-1.0, 1.0);

            if channels >= 2 {
                frame_out[0] = ((mix_l * 0.5 + 0.5) * u16::MAX as f32) as u16;
                frame_out[1] = ((mix_r * 0.5 + 0.5) * u16::MAX as f32) as u16;
            } else if channels == 1 {
                frame_out[0] = ((((mix_l + mix_r) * 0.5) * 0.5 + 0.5) * u16::MAX as f32) as u16;
            }
            for ch in frame_out.iter_mut().skip(2) {
                *ch = (0.5 * u16::MAX as f32) as u16;
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("DJ Engine starting...");

    let host = cpal::default_host();
    let device = host.default_output_device().expect("no output device available");
    let mut supported_configs_range = device.supported_output_configs()
        .expect("error while querying configs");
    let supported_config = supported_configs_range.next()
        .expect("no supported config?!")
        .with_max_sample_rate();

    let sample_format = supported_config.sample_format();
    let config: cpal::StreamConfig = supported_config.into();
    let channels = config.channels as usize;

    let (audio_tx, audio_rx) = rtrb::RingBuffer::new(4096);
    let (mut command_tx, command_rx) = rtrb::RingBuffer::new(64);

    let mut pipeline = AudioPipeline::new(audio_rx, command_rx);

    let err_fn = |err| eprintln!("an error occurred on stream: {}", err);

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            config.clone(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                pipeline.process_output_f32(data, channels);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_output_stream(
            config.clone(),
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                pipeline.process_output_i16(data, channels);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_output_stream(
            config.clone(),
            move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                pipeline.process_output_u16(data, channels);
            },
            err_fn,
            None,
        )?,
        sample_format => return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Unsupported sample format: {}", sample_format),
        ))),
    };

    stream.play()?;

    // Simulator thread for feeding audio and commands
    thread::spawn(move || {
        let mut audio_tx = audio_tx;
        let mut phase: f32 = 0.0;
        let mut cmd_timer = 0;

        loop {
            // Push some audio data
            while !audio_tx.is_full() {
                // simple sine waves to simulate different stems
                let v = phase.sin() * 0.1;
                let frame = StereoStemFrame {
                    drums: [v, v],
                    bass: [v * 0.5, v * 0.5],
                    instruments: [v * 0.2, v * 0.2],
                    vocals: [v * 0.8, v * 0.8],
                };
                let _ = audio_tx.push(frame);
                phase += 440.0 * 2.0 * std::f32::consts::PI / 44100.0;
                if phase > 2.0 * std::f32::consts::PI {
                    phase -= 2.0 * std::f32::consts::PI;
                }
            }

            cmd_timer += 1;
            if cmd_timer % 100 == 0 && !command_tx.is_full() {
                let _ = command_tx.push(ControlCommand::SetDrumsGain(0.5));
            }

            thread::sleep(Duration::from_millis(10));
        }
    });

    thread::sleep(Duration::from_secs(2));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stems_gain_control_mix() {
        let frame = StereoStemFrame {
            drums: [1.0, 0.5],
            bass: [0.5, 0.2],
            instruments: [0.2, 0.1],
            vocals: [0.8, 0.4],
        };

        let mut control = StemsGainControl::default();
        let mixed = control.mix(&frame);

        // Expected default: sum of all components
        // L: 1.0 + 0.5 + 0.2 + 0.8 = 2.5
        // R: 0.5 + 0.2 + 0.1 + 0.4 = 1.2
        assert_eq!(mixed, [2.5, 1.2]);

        // Modify gains
        control.drums_gain = 0.5;
        control.bass_gain = 0.0;
        control.instruments_gain = 2.0;
        control.vocals_gain = 1.0;

        let mixed2 = control.mix(&frame);
        // L: 1.0*0.5 + 0.5*0.0 + 0.2*2.0 + 0.8*1.0 = 0.5 + 0.0 + 0.4 + 0.8 = 1.7
        // R: 0.5*0.5 + 0.2*0.0 + 0.1*2.0 + 0.4*1.0 = 0.25 + 0.0 + 0.2 + 0.4 = 0.85
        assert_eq!(mixed2, [1.7, 0.85]);
    }

    #[test]
    fn test_audio_pipeline() {
        let (mut audio_tx, audio_rx) = rtrb::RingBuffer::new(2);
        let (mut command_tx, command_rx) = rtrb::RingBuffer::new(2);

        let mut pipeline = AudioPipeline::new(audio_rx, command_rx);

        // Push some audio
        let _ = audio_tx.push(StereoStemFrame {
            drums: [1.0, 1.0],
            bass: [0.0, 0.0],
            instruments: [0.0, 0.0],
            vocals: [0.0, 0.0],
        });

        // Push command
        let _ = command_tx.push(ControlCommand::SetDrumsGain(0.5));

        // Process buffer
        let mut output = [0.0; 2];
        pipeline.process_output_f32(&mut output, 2);

        // Verify command was processed and audio mixed correctly
        assert_eq!(output, [0.5, 0.5]);
        assert_eq!(pipeline.gain_control.drums_gain, 0.5);
    }

    #[test]
    fn test_audio_pipeline_underrun() {
        let (_audio_tx, audio_rx) = rtrb::RingBuffer::new(2);
        let (_command_tx, command_rx) = rtrb::RingBuffer::new(2);

        let mut pipeline = AudioPipeline::new(audio_rx, command_rx);

        let mut output = [1.0, 1.0]; // non-zero output buffer
        pipeline.process_output_f32(&mut output, 2);

        // Should pad with zeros
        assert_eq!(output, [0.0, 0.0]);
    }
}
