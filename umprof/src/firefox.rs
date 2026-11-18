//! Writes files that can be viewed on <https://profiler.firefox.com/>

use crate::model::{FrameKind, Profile};
use anyhow::Result;
use fxprof_processed_profile as fx;
use fxprof_processed_profile::{
    CategoryColor, CategoryHandle, CategoryPairHandle, CpuDelta, FrameFlags, FrameInfo, GraphColor,
    MarkerFieldFlags, MarkerFieldFormat, MarkerGraphType, MarkerLocations, MarkerTiming,
    ReferenceTimestamp, SamplingInterval, StaticSchemaMarker, StaticSchemaMarkerField,
    StaticSchemaMarkerGraph, StringHandle, ThreadHandle, Timestamp,
};
use rustc_hash::FxHashMap;
use std::io::Write;

pub const FILE_SUFFIX: &str = ".firefox.json";

pub fn write(profile: &Profile, output: impl Write) -> Result<()> {
    serde_json::to_writer(output, &build(profile))?;
    Ok(())
}

fn build(profile: &Profile) -> fx::Profile {
    let mut out = fx::Profile::new(
        &profile.name,
        ReferenceTimestamp::from_system_time(profile.start_time),
        SamplingInterval::from(profile.sample_interval),
    );
    out.set_os_name("Windows");
    // Prevent FF from attempting to look up symbols, as we already did this
    out.set_symbolicated(true);

    let categories = Categories::new(&mut out);
    let frames: Vec<FrameInfo> = profile
        .frames
        .iter()
        .map(|frame| FrameInfo {
            frame: fx::Frame::Label(out.intern_string(&frame.to_string())),
            category_pair: categories.of(frame.kind),
            flags: flags(frame.kind),
        })
        .collect();

    let start = Timestamp::from_nanos_since_reference(0);
    let process = out.add_process(&profile.name, profile.process_id, start);
    let cpu_per_sample = CpuDelta::from(profile.sample_interval);
    let mut handles = FxHashMap::default();
    let mut main_thread = None;
    let mut first_thread = None;

    for (index, thread) in profile.threads.iter().enumerate() {
        let first_sample = thread.samples.first().map_or(0, |s| s.time_ns);
        let handle = out.add_thread(
            process,
            thread.id,
            Timestamp::from_nanos_since_reference(first_sample),
            thread.is_main,
        );
        out.set_thread_name(handle, &thread.display_name());
        handles.insert(thread.id, handle);
        if thread.is_main {
            main_thread = Some(handle);
        }
        // We defined 0 to be the main thread
        if index == 0 {
            out.add_initial_selected_thread(handle);
            first_thread = Some(handle);
        }

        for sample in &thread.samples {
            // Our stack is in memory order (top first), but FF wants the
            // stack frames ordered bottom to top.
            let stack_frames = sample
                .stack
                .iter()
                .rev()
                .map(|&f| frames[f as usize].clone());
            let stack = out.intern_stack_frames(handle, stack_frames);
            let time = Timestamp::from_nanos_since_reference(sample.time_ns);
            out.add_sample(handle, time, stack, cpu_per_sample, 1);
        }
    }
    if !profile.presents.is_empty() {
        let category = out.add_category("Frames", CategoryColor::Green);
        add_present_markers(&mut out, profile, category, &handles, main_thread);
        if let Some(thread) = main_thread.or(first_thread) {
            add_frame_markers(&mut out, profile, category, thread);
        }
    }
    out
}

fn add_frame_markers(
    out: &mut fx::Profile,
    profile: &Profile,
    category: CategoryHandle,
    thread: ThreadHandle,
) {
    let name = out.intern_string("Frame rate");
    for pair in profile.presents.windows(2) {
        let (start_ns, end_ns) = (pair[0].start_ns, pair[1].start_ns);
        let marker = FrameMarker {
            name,
            category,
            frame_time_ms: frame_time_ms(Some(start_ns), end_ns),
        };
        let timing = MarkerTiming::Interval(
            Timestamp::from_nanos_since_reference(start_ns),
            Timestamp::from_nanos_since_reference(end_ns),
        );
        out.add_marker(thread, timing, marker);
    }
}

/// Marks each frame on the thread that presented it as well as on the main thread.
fn add_present_markers(
    out: &mut fx::Profile,
    profile: &Profile,
    category: CategoryHandle,
    handles: &FxHashMap<u32, ThreadHandle>,
    main_thread: Option<ThreadHandle>,
) {
    let name = out.intern_string("Present");
    let mut previous_start = None;

    for present in &profile.presents {
        let marker = PresentMarker {
            name,
            category,
            frame_time_ms: frame_time_ms(previous_start, present.start_ns),
        };
        previous_start = Some(present.start_ns);
        let timing = MarkerTiming::Interval(
            Timestamp::from_nanos_since_reference(present.start_ns),
            Timestamp::from_nanos_since_reference(present.end_ns),
        );

        let presenter = handles.get(&present.thread_id).copied();
        if let Some(thread) = presenter {
            out.add_marker(thread, timing.clone(), marker.clone());
        }
        if let Some(thread) = main_thread.filter(|&main| Some(main) != presenter) {
            out.add_marker(thread, timing, marker);
        }
    }
    for thread in handles.values() {
        out.set_thread_show_markers_in_timeline(*thread, true);
    }
}

fn frame_time_ms(previous_start_ns: Option<u64>, start_ns: u64) -> f64 {
    previous_start_ns.map_or(0.0, |previous| (start_ns - previous) as f64 / 1e6)
}

fn frame_rate(frame_time_ms: f64) -> f64 {
    if frame_time_ms > 0.0 {
        1000.0 / frame_time_ms
    } else {
        0.0
    }
}

#[derive(Debug, Clone)]
struct PresentMarker {
    name: StringHandle,
    category: CategoryHandle,
    frame_time_ms: f64,
}

impl StaticSchemaMarker for PresentMarker {
    const UNIQUE_MARKER_TYPE_NAME: &'static str = "Present";
    const DESCRIPTION: Option<&'static str> = Some(
        "A DXGI Present event, marking the point in time when the rendering thread has \
        finished uploading the rendering instructions for the current frame to the GPU.",
    );
    const LOCATIONS: MarkerLocations = MarkerLocations::MARKER_CHART
        .union(MarkerLocations::MARKER_TABLE)
        .union(MarkerLocations::TIMELINE_OVERVIEW);
    const TOOLTIP_LABEL: Option<&'static str> =
        Some("Present ({marker.data.frameTime} / {marker.data.frameRate} fps)");
    const TABLE_LABEL: Option<&'static str> = Some("Frame time {marker.data.frameTime}");
    const FIELDS: &'static [StaticSchemaMarkerField] = &[
        StaticSchemaMarkerField {
            key: "frameTime",
            label: "Frame time",
            // The profiler rounds its `milliseconds` format to 2 digits, which would show 20.43 as 20ms
            format: MarkerFieldFormat::Duration,
            flags: MarkerFieldFlags::empty(),
        },
        StaticSchemaMarkerField {
            key: "frameRate",
            label: "Frame rate",
            format: MarkerFieldFormat::Decimal,
            flags: MarkerFieldFlags::empty(),
        },
    ];

    fn name(&self, _profile: &mut fx::Profile) -> StringHandle {
        self.name
    }

    fn category(&self, _profile: &mut fx::Profile) -> CategoryHandle {
        self.category
    }

    fn string_field_value(&self, _field_index: u32) -> StringHandle {
        unreachable!("all fields are numbers")
    }

    fn number_field_value(&self, field_index: u32) -> f64 {
        match field_index {
            0 => self.frame_time_ms,
            _ => frame_rate(self.frame_time_ms),
        }
    }
}

#[derive(Debug, Clone)]
struct FrameMarker {
    name: StringHandle,
    category: CategoryHandle,
    frame_time_ms: f64,
}

impl StaticSchemaMarker for FrameMarker {
    const UNIQUE_MARKER_TYPE_NAME: &'static str = "Frame";
    const DESCRIPTION: Option<&'static str> =
        Some("Instantaneous frame rate, based on the frame time of the next Present event.");
    const LOCATIONS: MarkerLocations =
        MarkerLocations::MARKER_CHART.union(MarkerLocations::MARKER_TABLE);
    const TOOLTIP_LABEL: Option<&'static str> = Some("{marker.data.frameRate} fps");
    const TABLE_LABEL: Option<&'static str> = Some("{marker.data.frameRate} fps");
    const FIELDS: &'static [StaticSchemaMarkerField] = PresentMarker::FIELDS;
    const GRAPHS: &'static [StaticSchemaMarkerGraph] = &[StaticSchemaMarkerGraph {
        key: "frameRate",
        graph_type: MarkerGraphType::Line,
        color: Some(GraphColor::Green),
    }];

    fn name(&self, _profile: &mut fx::Profile) -> StringHandle {
        self.name
    }

    fn category(&self, _profile: &mut fx::Profile) -> CategoryHandle {
        self.category
    }

    fn string_field_value(&self, _field_index: u32) -> StringHandle {
        unreachable!("all fields are numbers")
    }

    fn number_field_value(&self, field_index: u32) -> f64 {
        match field_index {
            0 => self.frame_time_ms,
            _ => frame_rate(self.frame_time_ms),
        }
    }
}

/// Selects the right flags for this frame. We mark managed frames as JavaScript,
/// because the Firefox profiler lets us filter the stack to this.
fn flags(kind: FrameKind) -> FrameFlags {
    match kind {
        FrameKind::Managed { .. } => FrameFlags::IS_JS,
        _ => FrameFlags::empty(),
    }
}

struct Categories {
    managed: CategoryPairHandle,
    patched: CategoryPairHandle,
    mono: CategoryPairHandle,
    native: CategoryPairHandle,
    kernel: CategoryPairHandle,
    unknown: CategoryPairHandle,
}

impl Categories {
    fn new(out: &mut fx::Profile) -> Self {
        Self {
            managed: out.add_category("C#", CategoryColor::Yellow).into(),
            patched: out.add_category("Harmony", CategoryColor::Green).into(),
            mono: out.add_category("Mono", CategoryColor::Orange).into(),
            native: out.add_category("Native", CategoryColor::Blue).into(),
            kernel: out.add_category("Kernel", CategoryColor::Red).into(),
            unknown: out.add_category("Unidentified", CategoryColor::Gray).into(),
        }
    }

    fn of(&self, kind: FrameKind) -> CategoryPairHandle {
        match kind {
            FrameKind::Managed { is_patched: false } => self.managed,
            FrameKind::Managed { .. } => self.patched,
            FrameKind::Wrapper | FrameKind::Trampoline => self.mono,
            FrameKind::Native => self.native,
            FrameKind::Kernel => self.kernel,
            FrameKind::Unknown => self.unknown,
        }
    }
}
