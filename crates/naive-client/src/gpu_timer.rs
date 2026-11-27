//! GPU time per pass, from timestamp queries.
//!
//! Frame rate says little about headroom: with vsync it stops at the
//! display's refresh, and without it macOS still throttles a window that is
//! not in front. The timer measures the GPU's own work instead. Each timed
//! pass asks for a pair of timestamps, written at the start and end of the
//! pass (the only places Apple GPUs can sample them). Once a frame they are
//! resolved into a buffer that is read back when the GPU is done, a frame or
//! two later, like the swarm counters. Nothing waits on the GPU.
//!
//! It needs `TIMESTAMP_QUERY`; on a device without it the engine has no timer.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// Passes timed per frame, at most; later ones go untimed.
const MAX_SPANS: usize = 32;
/// Frames averaged.
const WINDOW: usize = 60;
/// Copies of the timestamps in flight.
const SLOTS: usize = 3;

const FREE: u8 = 0;
const PENDING: u8 = 1;
const READY: u8 = 2;

/// The device features the timer needs.
pub const FEATURES: wgpu::Features = wgpu::Features::TIMESTAMP_QUERY;

/// The name the swarm compute pass is timed under.
pub const SWARMS: &str = "swarms";
/// The splat sort's compute pass.
pub const SPLAT_SORT: &str = "splat sort";

/// Average GPU milliseconds per frame over the last frames measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuTimings {
    /// From the start of a frame's first timed pass to the end of its last.
    pub frame_ms: f32,
    /// Per pass name, in the order the passes ran, averaged over the frames
    /// it ran in. Passes with the same name in one frame are added up.
    /// Apple GPUs overlap consecutive passes, so these do not add up to
    /// `frame_ms`.
    pub passes: Vec<(String, f32)>,
    /// Frames the averages cover.
    pub frames: usize,
}

impl GpuTimings {
    pub fn pass_ms(&self, name: &str) -> Option<f32> {
        self.passes
            .iter()
            .find(|(n, _)| n == name)
            .map(|&(_, ms)| ms)
    }
}

struct Slot {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    names: Vec<String>,
}

pub struct GpuTimer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    slots: Vec<Slot>,
    /// Nanoseconds per timestamp tick.
    period_ns: f32,
    /// This frame's timed passes, in query order.
    frame: RefCell<Vec<String>>,
    /// Recent frames: the frame's span and each pass's milliseconds.
    history: VecDeque<(f32, Vec<(String, f32)>)>,
}

impl GpuTimer {
    /// A timer for `device`, if it has the features timestamps need.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(FEATURES) {
            return None;
        }
        let bytes = (MAX_SPANS * 2 * 8) as u64;
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("Pass timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: (MAX_SPANS * 2) as u32,
        });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Pass timestamps resolved"),
            size: bytes,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let slots = (0..SLOTS)
            .map(|_| Slot {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Pass timestamps readback"),
                    size: bytes,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(FREE)),
                names: Vec::new(),
            })
            .collect();
        Some(Self {
            queries,
            resolve,
            slots,
            period_ns: queue.get_timestamp_period(),
            frame: RefCell::new(Vec::new()),
            history: VecDeque::with_capacity(WINDOW),
        })
    }

    /// The next pair of queries, recorded under `name`.
    fn next_pair(&self, name: &str) -> Option<(u32, u32)> {
        let mut frame = self.frame.borrow_mut();
        if frame.len() >= MAX_SPANS {
            return None;
        }
        let k = frame.len() as u32;
        frame.push(name.to_string());
        Some((2 * k, 2 * k + 1))
    }

    /// Timestamps for a render pass named `name`.
    pub fn render_pass(&self, name: &str) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.next_pair(name)
            .map(|(begin, end)| wgpu::RenderPassTimestampWrites {
                query_set: &self.queries,
                beginning_of_pass_write_index: Some(begin),
                end_of_pass_write_index: Some(end),
            })
    }

    /// Timestamps for a compute pass named `name`.
    pub fn compute_pass(&self, name: &str) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        self.next_pair(name)
            .map(|(begin, end)| wgpu::ComputePassTimestampWrites {
                query_set: &self.queries,
                beginning_of_pass_write_index: Some(begin),
                end_of_pass_write_index: Some(end),
            })
    }

    /// Start averaging afresh.
    pub fn reset(&mut self) {
        self.history.clear();
    }

    /// After the frame's timed passes: record what has come back, then copy
    /// this frame's timestamps out to be read once the GPU is done.
    pub fn finish_frame(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let _ = device.poll(wgpu::PollType::Poll);
        self.collect();

        let names = std::mem::take(&mut *self.frame.borrow_mut());
        if names.is_empty() {
            return;
        }
        let Some(k) = self
            .slots
            .iter()
            .position(|s| s.state.load(Ordering::Acquire) == FREE)
        else {
            return;
        };
        let used = (names.len() * 2) as u32;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Resolve timestamps"),
        });
        encoder.resolve_query_set(&self.queries, 0..used, &self.resolve, 0);
        let slot = &mut self.slots[k];
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &slot.buffer, 0, u64::from(used) * 8);
        queue.submit(std::iter::once(encoder.finish()));
        slot.names = names;
        slot.state.store(PENDING, Ordering::Release);
        let state = slot.state.clone();
        slot.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                state.store(if result.is_ok() { READY } else { FREE }, Ordering::Release);
            });
    }

    fn collect(&mut self) {
        for slot in &mut self.slots {
            if slot.state.load(Ordering::Acquire) != READY {
                continue;
            }
            let Ok(view) = slot.buffer.slice(..).get_mapped_range() else {
                tracing::error!("GPU timer readback buffer was not mapped");
                continue;
            };
            let ticks: Vec<u64> = bytemuck::cast_slice(&view).to_vec();
            drop(view);
            slot.buffer.unmap();
            slot.state.store(FREE, Ordering::Release);
            let to_ms = |ticks: u64| ticks as f32 * self.period_ns / 1_000_000.0;
            let mut frame: Vec<(String, f32)> = Vec::new();
            let (mut first, mut last) = (u64::MAX, 0);
            for (k, name) in slot.names.drain(..).enumerate() {
                let (start, end) = (ticks[2 * k], ticks[2 * k + 1]);
                first = first.min(start);
                last = last.max(end);
                let ms = to_ms(end.saturating_sub(start));
                match frame.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, total)) => *total += ms,
                    None => frame.push((name, ms)),
                }
            }
            if self.history.len() == WINDOW {
                self.history.pop_front();
            }
            self.history
                .push_back((to_ms(last.saturating_sub(first)), frame));
        }
    }

    /// Averages over the last frames measured.
    pub fn timings(&self) -> GpuTimings {
        let frames = self.history.len();
        let mut passes: Vec<(String, f32, usize)> = Vec::new();
        let mut total = 0.0;
        for (span, frame) in &self.history {
            total += span;
            for (name, ms) in frame {
                match passes.iter_mut().find(|(n, _, _)| n == name) {
                    Some((_, sum, count)) => {
                        *sum += ms;
                        *count += 1;
                    }
                    None => passes.push((name.clone(), *ms, 1)),
                }
            }
        }
        GpuTimings {
            frame_ms: if frames > 0 {
                total / frames as f32
            } else {
                0.0
            },
            passes: passes
                .into_iter()
                .map(|(n, sum, count)| (n, sum / count as f32))
                .collect(),
            frames,
        }
    }
}
