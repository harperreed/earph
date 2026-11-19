// ABOUTME: Edits one local GraphIntent draft without submitting owner actions.
// ABOUTME: Preserves arbitrary graphs and rejects invalid raw edits transactionally.
use earph_core::graph_control::GraphIntent;
use earph_core::{
    audio::{ChannelLayout, PcmRepresentation},
    dsp_nodes::{PeqFilter, SrcPreset},
    graph::{
        Edge, FiniteGraphScalar, GraphDocument, GraphProcessingRequest, MAX_GRAPH_INTENT_BYTES,
        MAX_GRAPH_NODES, Node, NodeKind, PortFormat, QuantizationPolicy,
    },
    ids::NodeId,
    safety::SampleDomain,
};
use eframe::egui;
use std::sync::Arc;

pub(super) struct GraphIntentEditor {
    intent: GraphIntent,
    raw: String,
    error: Option<String>,
}

impl GraphIntentEditor {
    pub(super) fn intent(&self) -> &GraphIntent {
        &self.intent
    }
    pub(super) fn has_pending_raw_edits(&self) -> bool {
        self.raw != serde_json::to_string_pretty(&self.intent).expect("typed graph serialization")
    }
    pub(super) fn load(&mut self, intent: GraphIntent) {
        self.raw = serde_json::to_string_pretty(&intent).expect("typed graph serialization");
        self.intent = intent;
        self.error = None;
    }
    pub(super) fn add_fir(
        &mut self,
        asset: earph_core::dsp_nodes::FirAssetReference,
    ) -> Result<(), String> {
        if self.has_pending_raw_edits() {
            return Err("Accept or restore raw graph edits before adding a verified asset".into());
        }
        let order = chain_order(&self.intent.graph).ok_or(
            "Adding an asset requires one serial graph; edit arbitrary topology explicitly in JSON",
        )?;
        let predecessor = self.intent.graph.nodes[order[order.len() - 2]].format;
        if predecessor.sample_rate_hz != asset.sample_rate_hz
            || predecessor.channels != asset.channels
        {
            return Err("Verified FIR rate/layout differs from this graph; no resampling or broadcasting is available".into());
        }
        if self.intent.graph.nodes.len() >= MAX_GRAPH_NODES {
            return Err("Graph node limit reached".into());
        }
        let mut candidate = self.intent.clone();
        edit_chain(
            &mut candidate,
            &order,
            ChainAction::Add(NodeKind::Convolution { asset }),
        );
        candidate
            .graph
            .validate_structure()
            .map_err(|e| e.to_string())?;
        self.load(candidate);
        Ok(())
    }
    pub(super) fn show(&mut self, ui: &mut egui::Ui) -> bool {
        let before = self.intent.clone();
        ui.label("Local graph draft. Saving and live owner validation are separate actions.");
        let dirty = self.has_pending_raw_edits();
        if dirty {
            ui.label("Raw edits pending. Accept or restore them before using common controls.");
        }
        if let Some(order) = chain_order(&self.intent.graph) {
            let mut candidate = self.intent.clone();
            ui.add_enabled_ui(!dirty, |ui| {
                processing(ui, &mut candidate.processing);
                let mut action = None;
                for (position, index) in order.iter().copied().enumerate() {
                    let node = &mut Arc::make_mut(&mut candidate.graph).nodes[index];
                    let node_id = node.id.clone();
                    ui.push_id(node_id.as_str(), |ui| {
                        ui.group(|ui| {
                            ui.label(format!("{}: {}", node.id.as_str(), kind_name(&node.kind)));
                            ui.label("Stable node ID (edit IDs and edge ports in JSON)");
                            port_format(ui, node);
                            parameters(ui, node);
                            let internal = position > 0 && position + 1 < order.len();
                            ui.horizontal(|ui| {
                                if ui.add_enabled(
                                    internal && position > 1,
                                    egui::Button::new(format!("Move {} up", node.id.as_str())),
                                ).clicked() {
                                    action = Some(ChainAction::Move(position, position - 1));
                                }
                                if ui.add_enabled(
                                    internal && position + 2 < order.len(),
                                    egui::Button::new(format!("Move {} down", node.id.as_str())),
                                ).clicked() {
                                    action = Some(ChainAction::Move(position, position + 1));
                                }
                                if ui.add_enabled(
                                    internal,
                                    egui::Button::new(format!("Remove {}", node.id.as_str())),
                                ).clicked() {
                                    action = Some(ChainAction::Remove(position));
                                }
                            });
                        });
                    });
                }
                ui.label("Input and Output are the chain endpoints. Edit their identity or topology in JSON.");
                ui.horizontal_wrapped(|ui| {
                    for (label, kind) in common_kinds() {
                        if ui.add_enabled(
                            candidate.graph.nodes.len() < MAX_GRAPH_NODES,
                            egui::Button::new(format!("Add {label}")),
                        ).clicked() {
                            action = Some(ChainAction::Add(kind));
                        }
                    }
                });
                ui.label("Convolution: verify and review an asset in Correction source, then add it explicitly. Existing references remain editable in JSON.");
                ui.label("Sum uses one input here. Use JSON for multiple inputs and explicit ports.");
                ui.label("Changing nodes does not adjust processing precision or other port formats. Owner validation checks compatibility.");
                if let Some(action) = action {
                    edit_chain(&mut candidate, &order, action);
                }
            });
            if candidate != self.intent {
                self.replace(candidate);
            }
        } else {
            ui.label("Raw mode required: this graph is not one connected Input-to-Output chain with port zero edges. All nodes and ports remain intact.");
        }
        ui.separator();
        let label = ui.label("GraphIntent JSON");
        ui.add(
            egui::TextEdit::multiline(&mut self.raw)
                .id_salt("graph-intent-json")
                .code_editor()
                .char_limit(MAX_GRAPH_INTENT_BYTES)
                .desired_width(f32::INFINITY)
                .desired_rows(16),
        )
        .labelled_by(label.id);
        ui.push_id("graph-raw-actions", |ui| {
            ui.horizontal(|ui| {
                if ui.button("Accept raw edits").clicked() {
                    self.accept_raw();
                }
                if ui.button("Restore canonical JSON").clicked() {
                    self.load(self.intent.clone());
                }
            });
        });
        if let Some(error) = &self.error {
            ui.label(format!("Graph draft error: {error}"));
        }
        self.intent != before
    }
    fn accept_raw(&mut self) -> bool {
        if self.raw.len() > MAX_GRAPH_INTENT_BYTES {
            self.error = Some("Graph JSON exceeds this editor's 128 KiB limit".into());
            return false;
        }
        match serde_json::from_str::<GraphIntent>(&self.raw) {
            Ok(intent) => self.replace(intent),
            Err(error) => {
                self.error = Some(error.to_string());
                false
            }
        }
    }
    fn replace(&mut self, intent: GraphIntent) -> bool {
        if let Err(error) = intent.graph.validate_structure() {
            self.error = Some(error.to_string());
            return false;
        }
        let changed = self.intent != intent;
        self.load(intent);
        changed
    }
}

impl Default for GraphIntentEditor {
    fn default() -> Self {
        let format = PortFormat {
            domain: SampleDomain::Pcm,
            sample_rate_hz: 48000,
            channels: ChannelLayout::Stereo,
            precision: PcmRepresentation::Signed24,
            max_quantum_frames: 256,
        };
        let input = NodeId::new("in").expect("static node ID");
        let output = NodeId::new("out").expect("static node ID");
        let intent = GraphIntent {
            graph: Arc::new(GraphDocument {
                schema_version: 1,
                nodes: vec![
                    Node {
                        id: input.clone(),
                        kind: NodeKind::Input,
                        format,
                    },
                    Node {
                        id: output.clone(),
                        kind: NodeKind::Output,
                        format,
                    },
                ],
                edges: vec![Edge {
                    from: input,
                    to: output.clone(),
                    from_port: 0,
                    to_port: 0,
                }],
                output,
            }),
            processing: GraphProcessingRequest::IntegerBypass,
        };
        Self {
            raw: serde_json::to_string_pretty(&intent).expect("static graph serialization"),
            intent,
            error: None,
        }
    }
}

/// Recognize topology without changing authored node or edge ordering.
fn chain_order(graph: &GraphDocument) -> Option<Vec<usize>> {
    if graph.nodes.len() < 2 || graph.edges.len() + 1 != graph.nodes.len() {
        return None;
    }
    if graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Input))
        .count()
        != 1
        || graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Output))
            .count()
            != 1
    {
        return None;
    }
    for (index, node) in graph.nodes.iter().enumerate() {
        if graph.nodes[..index].iter().any(|n| n.id == node.id) {
            return None;
        }
    }
    let mut order = vec![
        graph
            .nodes
            .iter()
            .position(|n| matches!(n.kind, NodeKind::Input))?,
    ];
    loop {
        let node = &graph.nodes[*order.last()?];
        if matches!(node.kind, NodeKind::Output) {
            return (node.id == graph.output && order.len() == graph.nodes.len()).then_some(order);
        }
        let mut outgoing = graph.edges.iter().filter(|edge| edge.from == node.id);
        let edge = outgoing.next()?;
        if outgoing.next().is_some() || edge.from_port != 0 || edge.to_port != 0 {
            return None;
        }
        let next = graph.nodes.iter().position(|n| n.id == edge.to)?;
        if order.contains(&next) {
            return None;
        }
        order.push(next);
    }
}

enum ChainAction {
    Add(NodeKind),
    Move(usize, usize),
    Remove(usize),
}

fn edit_chain(intent: &mut GraphIntent, order: &[usize], action: ChainAction) {
    let graph = Arc::make_mut(&mut intent.graph);
    let mut nodes: Vec<Node> = order
        .iter()
        .map(|&index| graph.nodes[index].clone())
        .collect();
    match action {
        ChainAction::Add(mut kind) => {
            let id = (1..)
                .map(|n| NodeId::new(format!("node-{n}")).expect("generated node ID"))
                .find(|id| nodes.iter().all(|n| n.id != *id))
                .expect("bounded graph has unused ID");
            let format = nodes[nodes.len() - 2].format;
            match &mut kind {
                NodeKind::Matrix {
                    input_channels,
                    coefficients,
                } => {
                    *input_channels = format.channels;
                    if format.channels == ChannelLayout::Mono {
                        *coefficients =
                            vec![FiniteGraphScalar::new(1.0).expect("finite identity coefficient")];
                    }
                }
                NodeKind::Src { input_rate_hz, .. } => *input_rate_hz = format.sample_rate_hz,
                _ => {}
            }
            nodes.insert(nodes.len() - 1, Node { id, kind, format });
        }
        ChainAction::Move(from, to) => nodes.swap(from, to),
        ChainAction::Remove(position) => {
            nodes.remove(position);
        }
    }
    graph.edges = nodes
        .windows(2)
        .map(|pair| Edge {
            from: pair[0].id.clone(),
            to: pair[1].id.clone(),
            from_port: 0,
            to_port: 0,
        })
        .collect();
    graph.nodes = nodes;
}

fn choose<T: Copy + Eq>(ui: &mut egui::Ui, label: &str, value: &mut T, options: &[(T, &str)]) {
    ui.horizontal(|ui| {
        let label_id = ui.label(label).id;
        egui::ComboBox::from_id_salt(label)
            .selected_text(
                options
                    .iter()
                    .find(|(v, _)| v == value)
                    .map_or("Unknown", |(_, s)| *s),
            )
            .show_ui(ui, |ui| {
                for (option, name) in options {
                    ui.selectable_value(value, *option, *name);
                }
            })
            .response
            .labelled_by(label_id);
    });
}

fn integer<T: ToString + std::str::FromStr>(ui: &mut egui::Ui, label: &str, value: &mut T) {
    ui.horizontal(|ui| {
        let label = ui.label(label);
        let mut text=value.to_string();
        let response=ui.add(egui::TextEdit::singleline(&mut text).char_limit(20).desired_width(180.0)).labelled_by(label.id);
        if response.changed() {
            match text.parse() {
                Ok(parsed)=>*value=parsed,
                Err(_)=>{ui.label("Enter a nonnegative whole number within the field's integer range. The previous value is retained.");}
            }
        }
    });
}
fn scalar(ui: &mut egui::Ui, label: &str, value: &mut FiniteGraphScalar) {
    let mut v = value.get();
    ui.horizontal(|ui| {
        let label = ui.label(label);
        ui.add(egui::DragValue::new(&mut v).custom_formatter(|value, _| format!("{value:?}")))
            .labelled_by(label.id);
    });
    match FiniteGraphScalar::new(v) {
        Ok(v) => *value = v,
        Err(_) => {
            ui.label("Value must be finite; the previous value is retained.");
        }
    }
}
fn processing(ui: &mut egui::Ui, value: &mut GraphProcessingRequest) {
    choose(
        ui,
        "Requested processing precision",
        value,
        &[
            (GraphProcessingRequest::IntegerBypass, "IntegerBypass"),
            (
                GraphProcessingRequest::Float64 {
                    quantization: QuantizationPolicy::RoundTiesAwaySaturateNoDither,
                },
                "Float64",
            ),
        ],
    );
    ui.label(match value {
        GraphProcessingRequest::IntegerBypass => "Quantization: none requested (IntegerBypass)",
        GraphProcessingRequest::Float64 { .. } => {
            "Quantization policy: RoundTiesAwaySaturateNoDither"
        }
    });
}
fn channels(ui: &mut egui::Ui, label: &str, value: &mut ChannelLayout) {
    choose(
        ui,
        label,
        value,
        &[
            (ChannelLayout::Mono, "Mono"),
            (ChannelLayout::Stereo, "Stereo"),
        ],
    );
}
fn port_format(ui: &mut egui::Ui, node: &mut Node) {
    let label = |field: &str| format!("{}: {field}", node.id.as_str());
    let format = &mut node.format;
    let matrix = matches!(node.kind, NodeKind::Matrix { .. });
    choose(
        ui,
        &label("Sample domain"),
        &mut format.domain,
        &[(SampleDomain::Pcm, "PCM"), (SampleDomain::Dsd, "DSD")],
    );
    integer(ui, &label("Sample rate Hz"), &mut format.sample_rate_hz);
    if matrix {
        ui.label(format!(
            "{}: {:?}. Use the matrix reset controls or JSON to change dimensions.",
            label("Output channels"),
            format.channels
        ));
    } else {
        channels(ui, &label("Output channels"), &mut format.channels);
    }
    choose(
        ui,
        &label("Port precision"),
        &mut format.precision,
        &[
            (PcmRepresentation::Signed16, "Signed16"),
            (PcmRepresentation::Signed24, "Signed24"),
            (PcmRepresentation::Signed32, "Signed32"),
            (PcmRepresentation::Float64, "Float64"),
        ],
    );
    integer(
        ui,
        &label("Maximum quantum frames"),
        &mut format.max_quantum_frames,
    );
}
fn parameters(ui: &mut egui::Ui, node: &mut Node) {
    let node_id = node.id.as_str().to_owned();
    let label = |field: &str| format!("{node_id}: {field}");
    match &mut node.kind {
        NodeKind::Gain { gain_linear } => scalar(ui, &label("Linear gain"), gain_linear),
        NodeKind::Peq { filter } => match filter {
            PeqFilter::Peaking {
                frequency_hz,
                q,
                gain_db,
            }
            | PeqFilter::LowShelfQ {
                frequency_hz,
                q,
                gain_db,
            }
            | PeqFilter::HighShelfQ {
                frequency_hz,
                q,
                gain_db,
            } => {
                scalar(ui, &label("Frequency Hz"), frequency_hz);
                scalar(ui, &label("Q"), q);
                scalar(ui, &label("Gain dB"), gain_db);
            }
            PeqFilter::LowShelf {
                frequency_hz,
                gain_db,
            }
            | PeqFilter::HighShelf {
                frequency_hz,
                gain_db,
            } => {
                scalar(ui, &label("Frequency Hz"), frequency_hz);
                scalar(ui, &label("Gain dB"), gain_db);
            }
        },
        NodeKind::Delay { delay_frames } => integer(ui, &label("Delay frames"), delay_frames),
        NodeKind::Src {
            input_rate_hz,
            preset,
            chunk_frames,
        } => {
            integer(ui, &label("Input rate Hz"), input_rate_hz);
            choose(
                ui,
                &label("SRC preset"),
                preset,
                &[
                    (SrcPreset::Quality, "Quality"),
                    (SrcPreset::Responsive, "Responsive"),
                ],
            );
            integer(ui, &label("Chunk frames"), chunk_frames);
        }
        NodeKind::Matrix {
            input_channels,
            coefficients,
        } => {
            ui.label(format!("Matrix input channels: {input_channels:?}. Coefficients are output rows by input columns."));
            ui.label("Resetting matrix dimensions sets every coefficient to zero. Edit JSON to preserve authored coefficients when changing dimensions.");
            for input in [ChannelLayout::Mono, ChannelLayout::Stereo] {
                for output in [ChannelLayout::Mono, ChannelLayout::Stereo] {
                    if ui
                        .button(format!(
                            "{node_id}: Reset matrix to {input:?} input / {output:?} output"
                        ))
                        .clicked()
                    {
                        *input_channels = input;
                        node.format.channels = output;
                        *coefficients = vec![
                            FiniteGraphScalar::new(0.0)
                                .expect("finite zero coefficient");
                            input.count() * output.count()
                        ];
                    }
                }
            }
            for (index, value) in coefficients.iter_mut().enumerate() {
                ui.push_id(index, |ui| {
                    scalar(ui, &label(&format!("Coefficient {index}")), value)
                });
            }
        }
        NodeKind::Convolution { asset } => {
            ui.label(format!(
                "Existing FIR reference: {} | {} Hz | {:?} | {} taps per channel | {:?}",
                asset.sha256.as_str(),
                asset.sample_rate_hz,
                asset.channels,
                asset.taps_per_channel,
                asset.purpose
            ));
            ui.label("Edit the exact reference in JSON. Asset availability is unknown until owner validation.");
        }
        NodeKind::Input | NodeKind::Output | NodeKind::Bypass | NodeKind::Sum | NodeKind::Meter => {
        }
    }
}
fn common_kinds() -> Vec<(&'static str, NodeKind)> {
    let finite = |value| FiniteGraphScalar::new(value).expect("finite default parameter");
    let frequency_hz = finite(1000.0);
    let q = finite(1.0);
    let gain_db = finite(0.0);
    vec![
        ("Bypass", NodeKind::Bypass),
        (
            "Gain",
            NodeKind::Gain {
                gain_linear: finite(1.0),
            },
        ),
        (
            "Peaking",
            NodeKind::Peq {
                filter: PeqFilter::Peaking {
                    frequency_hz,
                    q,
                    gain_db,
                },
            },
        ),
        (
            "LowShelf",
            NodeKind::Peq {
                filter: PeqFilter::LowShelf {
                    frequency_hz,
                    gain_db,
                },
            },
        ),
        (
            "HighShelf",
            NodeKind::Peq {
                filter: PeqFilter::HighShelf {
                    frequency_hz,
                    gain_db,
                },
            },
        ),
        (
            "LowShelfQ",
            NodeKind::Peq {
                filter: PeqFilter::LowShelfQ {
                    frequency_hz,
                    q,
                    gain_db,
                },
            },
        ),
        (
            "HighShelfQ",
            NodeKind::Peq {
                filter: PeqFilter::HighShelfQ {
                    frequency_hz,
                    q,
                    gain_db,
                },
            },
        ),
        (
            "SRC",
            NodeKind::Src {
                input_rate_hz: 48000,
                preset: SrcPreset::Quality,
                chunk_frames: 256,
            },
        ),
        ("Delay", NodeKind::Delay { delay_frames: 0 }),
        (
            "Matrix",
            NodeKind::Matrix {
                input_channels: ChannelLayout::Stereo,
                coefficients: vec![finite(1.0), finite(0.0), finite(0.0), finite(1.0)],
            },
        ),
        ("Sum", NodeKind::Sum),
        ("Meter", NodeKind::Meter),
    ]
}
fn kind_name(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Input => "Input",
        NodeKind::Output => "Output",
        NodeKind::Convolution { .. } => "Convolution",
        NodeKind::Peq { .. } => "PEQ",
        NodeKind::Src { .. } => "SRC",
        NodeKind::Bypass => "Bypass",
        NodeKind::Gain { .. } => "Gain",
        NodeKind::Delay { .. } => "Delay",
        NodeKind::Matrix { .. } => "Matrix",
        NodeKind::Sum => "Sum",
        NodeKind::Meter => "Meter",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn verified_fir_insertion_checks_its_actual_predecessor_and_preserves_the_draft_on_failure() {
        let mut editor = super::GraphIntentEditor::default();
        let mut intent = editor.intent().clone();
        let input = std::sync::Arc::make_mut(&mut intent.graph)
            .nodes
            .iter_mut()
            .find(|n| matches!(n.kind, earph_core::graph::NodeKind::Input))
            .unwrap();
        input.format.sample_rate_hz = 44100;
        editor.load(intent.clone());
        let asset = earph_core::dsp_nodes::FirAssetReference {
            sha256: earph_core::dsp_nodes::FirSha256::new("a".repeat(64)).unwrap(),
            sample_rate_hz: 48000,
            channels: earph_core::audio::ChannelLayout::Stereo,
            taps_per_channel: 1,
            purpose: earph_core::dsp_nodes::FirPurpose::Correction,
        };
        assert!(
            editor.add_fir(asset).is_err(),
            "mismatched predecessor must not acquire a convolution node"
        );
        assert_eq!(editor.intent(), &intent);
    }
    #[test]
    fn verified_fir_insertion_uses_the_canonical_chain_and_keeps_processing_explicit() {
        let mut editor = GraphIntentEditor::default();
        let processing = editor.intent().processing;
        let asset = earph_core::dsp_nodes::FirAssetReference {
            sha256: earph_core::dsp_nodes::FirSha256::new("b".repeat(64)).unwrap(),
            sample_rate_hz: 48000,
            channels: ChannelLayout::Stereo,
            taps_per_channel: 1,
            purpose: earph_core::dsp_nodes::FirPurpose::Correction,
        };
        editor.add_fir(asset.clone()).unwrap();
        let consumed: GraphIntent = serde_json::from_str(&editor.raw).unwrap();
        assert_eq!(&consumed, editor.intent());
        assert_eq!(consumed.processing, processing);
        assert_eq!(consumed.graph.nodes.len(), 3);
        assert!(consumed.graph.nodes.iter().any(|node| node.kind
            == NodeKind::Convolution {
                asset: asset.clone()
            }));
        assert_eq!(consumed.graph.edges.len(), 2);
    }
    use super::*;
    use serde_json::json;

    fn fixture() -> GraphIntent {
        serde_json::from_value(json!({
            "graph": {"schema_version":1,"nodes":[
                {"id":"in","kind":{"kind":"input"},"format":format()},
                {"id":"out","kind":{"kind":"output"},"format":format()}
            ],"edges":[{"from":"in","to":"out","from_port":0,"to_port":0}],"output":"out"},
            "processing":{"kind":"integer_bypass"}
        }))
        .unwrap()
    }
    fn format() -> serde_json::Value {
        json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo", "precision":"signed24","max_quantum_frames":256})
    }
    fn editor() -> GraphIntentEditor {
        let intent = fixture();
        GraphIntentEditor {
            raw: serde_json::to_string_pretty(&intent).unwrap(),
            intent,
            error: None,
        }
    }
    fn frame(
        context: &egui::Context,
        editor: &mut GraphIntentEditor,
        events: Vec<egui::Event>,
    ) -> egui::accesskit::TreeUpdate {
        let mut output = context.run_ui(
            egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1800.0, 8000.0),
                )),
                ..Default::default()
            },
            |ui| {
                editor.show(ui);
            },
        );
        output.textures_delta.clear();
        output.platform_output.accesskit_update.unwrap()
    }
    fn named(tree: &egui::accesskit::TreeUpdate, name: &str) -> egui::accesskit::NodeId {
        tree.nodes
            .iter()
            .find(|(_, node)| {
                node.role() != egui::accesskit::Role::TextRun
                    && node.label().or_else(|| node.value()) == Some(name)
            })
            .unwrap_or_else(|| panic!("missing {name}"))
            .0
    }
    fn click(
        context: &egui::Context,
        editor: &mut GraphIntentEditor,
        tree: &egui::accesskit::TreeUpdate,
        name: &str,
    ) -> egui::accesskit::TreeUpdate {
        frame(
            context,
            editor,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Click,
                    target_tree: tree.tree_id,
                    target_node: named(tree, name),
                    data: None,
                },
            )],
        )
    }

    #[test]
    fn raw_accept_uses_core_deserializer_and_preserves_last_valid_draft() {
        let mut editor = editor();
        let before = editor.intent().clone();
        editor.raw = "{invalid".into();
        assert!(!editor.accept_raw());
        assert_eq!(editor.intent(), &before);
        assert_eq!(editor.raw, "{invalid");
        assert!(editor.error.is_some());
        let mut value = serde_json::to_value(&before).unwrap();
        value["graph"]["nodes"][0]["format"]["sample_rate_hz"] = json!(96000);
        editor.raw = serde_json::to_string(&value).unwrap();
        assert!(editor.accept_raw());
        assert_eq!(editor.intent().graph.nodes[0].format.sample_rate_hz, 96000);
        assert!(editor.error.is_none());
    }

    #[test]
    fn renderer_adds_reorders_and_removes_real_chain_nodes() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut editor = editor();
        let tree = frame(&context, &mut editor, vec![]);
        click(&context, &mut editor, &tree, "Add Gain");
        assert_eq!(editor.intent().graph.nodes.len(), 3);
        let tree = frame(&context, &mut editor, vec![]);
        click(&context, &mut editor, &tree, "Add Delay");
        let tree = frame(&context, &mut editor, vec![]);
        click(&context, &mut editor, &tree, "Move node-2 up");
        assert_eq!(editor.intent().graph.edges[0].to.as_str(), "node-2");
        let tree = frame(&context, &mut editor, vec![]);
        click(&context, &mut editor, &tree, "Remove node-1");
        let decoded: GraphIntent = serde_json::from_str(&editor.raw).unwrap();
        assert_eq!(&decoded, editor.intent());
        assert_eq!(decoded.graph.nodes.len(), 3);
        assert_eq!(decoded.graph.edges[1].to.as_str(), "out");
    }

    #[test]
    fn imported_branches_remain_exact_and_never_offer_chain_mutation() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["graph"]["nodes"].as_array_mut().unwrap().insert(
            1,
            json!({"id":"sum","kind":{"kind":"sum"},"format":format()}),
        );
        value["graph"]["edges"] = json!([
            {"from":"in","to":"sum","from_port":0,"to_port":0},
            {"from":"in","to":"sum","from_port":0,"to_port":1},
            {"from":"sum","to":"out","from_port":0,"to_port":0}
        ]);
        let intent: GraphIntent = serde_json::from_value(value).unwrap();
        let mut editor = editor();
        editor.load(intent.clone());
        let context = egui::Context::default();
        context.enable_accesskit();
        let tree = frame(&context, &mut editor, vec![]);
        assert_eq!(editor.intent(), &intent);
        assert_eq!(
            serde_json::from_str::<GraphIntent>(&editor.raw).unwrap(),
            intent
        );
        assert!(tree.nodes.iter().any(|(_, node)| {
            node.value()
                .or_else(|| node.label())
                .is_some_and(|s| s.contains("Raw mode required"))
        }));
        assert!(
            !tree
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("Add Gain") && !node.is_disabled())
        );
    }

    #[test]
    fn raw_rejects_unknown_fields_nonfinite_and_excess_storage_without_replacing_intent() {
        let mut editor = editor();
        let before = editor.intent().clone();
        let mut value = serde_json::to_value(&before).unwrap();
        value["graph"]["nodes"][0]["kind"]["unexpected"] = json!(true);
        for raw in [
            serde_json::to_string(&value).unwrap(),
            " ".repeat(128 * 1024 + 1),
            editor.raw.replace("48000", "1e400"),
        ] {
            editor.raw = raw.clone();
            assert!(!editor.accept_raw());
            assert_eq!(editor.intent(), &before);
            assert_eq!(editor.raw, raw);
            assert!(editor.error.is_some());
        }
    }

    #[test]
    fn raw_widget_accepts_only_explicit_valid_edits_and_restores_canonical_on_request() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut editor = editor();
        let before = editor.intent().clone();
        let tree = frame(&context, &mut editor, vec![]);
        let label = named(&tree, "GraphIntent JSON");
        let entry = tree
            .nodes
            .iter()
            .find(|(_, node)| node.labelled_by().contains(&label))
            .unwrap()
            .0;
        let event = egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::SetValue,
            target_tree: tree.tree_id,
            target_node: entry,
            data: Some(egui::accesskit::ActionData::Value("{bad".into())),
        });
        frame(&context, &mut editor, vec![event]);
        assert_eq!(editor.intent(), &before);
        assert_eq!(editor.raw, "{bad");
        let tree = frame(&context, &mut editor, vec![]);
        let tree = click(&context, &mut editor, &tree, "Accept raw edits");
        assert_eq!(editor.intent(), &before);
        assert!(editor.error.is_some());
        click(&context, &mut editor, &tree, "Restore canonical JSON");
        assert_eq!(
            serde_json::from_str::<GraphIntent>(&editor.raw).unwrap(),
            before
        );
        let tree = frame(&context, &mut editor, vec![]);
        click(&context, &mut editor, &tree, "Add Peaking");
        assert_eq!(editor.intent().graph.nodes.len(), 3);
        let tree = frame(&context, &mut editor, vec![]);
        assert!(
            tree.nodes
                .iter()
                .any(|(_, node)| node.label().or_else(|| node.value())
                    == Some("node-1: Frequency Hz"))
        );
    }

    fn set_value(
        context: &egui::Context,
        editor: &mut GraphIntentEditor,
        tree: &egui::accesskit::TreeUpdate,
        label: &str,
        value: &str,
    ) -> egui::accesskit::TreeUpdate {
        let label = named(tree, label);
        let entry = tree
            .nodes
            .iter()
            .find(|(_, node)| node.labelled_by().contains(&label))
            .unwrap();
        let data = if entry.1.numeric_value().is_some() {
            egui::accesskit::ActionData::NumericValue(value.parse().unwrap())
        } else {
            egui::accesskit::ActionData::Value(value.into())
        };
        frame(
            context,
            editor,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::SetValue,
                    target_tree: tree.tree_id,
                    target_node: entry.0,
                    data: Some(data),
                },
            )],
        )
    }

    #[test]
    fn structured_parameters_reach_the_real_core_json_consumer() {
        for (button, fields) in [
            ("Add Gain", vec![("Linear gain", "0.625", "gain_linear")]),
            (
                "Add Peaking",
                vec![
                    ("Frequency Hz", "1733.125", "filter.frequency_hz"),
                    ("Q", "0.73", "filter.q"),
                    ("Gain dB", "-5.375", "filter.gain_db"),
                ],
            ),
            (
                "Add LowShelf",
                vec![
                    ("Frequency Hz", "1733.125", "filter.frequency_hz"),
                    ("Gain dB", "-5.375", "filter.gain_db"),
                ],
            ),
            (
                "Add HighShelf",
                vec![("Gain dB", "-5.375", "filter.gain_db")],
            ),
            ("Add LowShelfQ", vec![("Q", "0.73", "filter.q")]),
            ("Add HighShelfQ", vec![("Q", "0.73", "filter.q")]),
            (
                "Add SRC",
                vec![
                    ("Input rate Hz", "44100", "input_rate_hz"),
                    ("Chunk frames", "512", "chunk_frames"),
                ],
            ),
            ("Add Delay", vec![("Delay frames", "923", "delay_frames")]),
            (
                "Add Matrix",
                vec![("Coefficient 1", "-0.125", "coefficients.1")],
            ),
        ] {
            let context = egui::Context::default();
            context.enable_accesskit();
            let mut editor = editor();
            let tree = frame(&context, &mut editor, vec![]);
            click(&context, &mut editor, &tree, button);
            let mut tree = frame(&context, &mut editor, vec![]);
            for (label, text, path) in fields {
                tree = set_value(
                    &context,
                    &mut editor,
                    &tree,
                    &format!("node-1: {label}"),
                    text,
                );
                let actual: GraphIntent = serde_json::from_str(&editor.raw).unwrap();
                let value = serde_json::to_value(
                    actual
                        .graph
                        .nodes
                        .iter()
                        .find(|n| n.id.as_str() == "node-1")
                        .unwrap(),
                )
                .unwrap();
                let mut field = &value["kind"];
                for part in path.split('.') {
                    field = if let Ok(index) = part.parse::<usize>() {
                        &field[index]
                    } else {
                        &field[part]
                    };
                }
                assert_eq!(
                    field.as_f64(),
                    Some(text.parse::<f64>().unwrap()),
                    "{button} {label}"
                );
            }
        }
    }

    #[test]
    fn mono_matrix_insertion_uses_the_authored_channel_count() {
        let mut intent = fixture();
        for node in &mut Arc::make_mut(&mut intent.graph).nodes {
            node.format.channels = ChannelLayout::Mono;
        }
        let mut editor = editor();
        editor.load(intent);
        let context = egui::Context::default();
        context.enable_accesskit();
        let tree = frame(&context, &mut editor, vec![]);
        click(&context, &mut editor, &tree, "Add Matrix");
        let node = editor
            .intent()
            .graph
            .nodes
            .iter()
            .find(|n| n.id.as_str() == "node-1")
            .expect("matrix insertion rejected");
        assert!(
            matches!(&node.kind,NodeKind::Matrix{input_channels:ChannelLayout::Mono,coefficients} if coefficients.len()==1)
        );
        assert!(editor.intent().graph.validate_structure().is_ok());
    }

    #[test]
    fn explicit_matrix_dimension_reset_keeps_a_matching_coefficient_shape() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut editor = editor();
        let tree = frame(&context, &mut editor, vec![]);
        click(&context, &mut editor, &tree, "Add Matrix");
        let tree = frame(&context, &mut editor, vec![]);
        click(
            &context,
            &mut editor,
            &tree,
            "node-1: Reset matrix to Mono input / Stereo output",
        );
        let node = editor
            .intent()
            .graph
            .nodes
            .iter()
            .find(|n| n.id.as_str() == "node-1")
            .unwrap();
        assert_eq!(node.format.channels, ChannelLayout::Stereo);
        assert!(
            matches!(&node.kind,NodeKind::Matrix{input_channels:ChannelLayout::Mono,coefficients} if coefficients.len()==2 && coefficients.iter().all(|v|v.get()==0.0))
        );
        assert!(editor.intent().graph.validate_structure().is_ok());
    }

    #[test]
    fn no_edit_frame_preserves_arbitrary_storage_order_and_large_delay_integer() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["graph"]["nodes"].as_array_mut().unwrap().insert(0,json!({"id":"delay","kind":{"kind":"delay","delay_frames":9007199254740993_u64},"format":format()}));
        value["graph"]["edges"] = json!([
            {"from":"delay","to":"out","from_port":0,"to_port":0},
            {"from":"in","to":"delay","from_port":0,"to_port":0}
        ]);
        let intent: GraphIntent = serde_json::from_value(value).unwrap();
        let mut editor = editor();
        editor.load(intent.clone());
        let context = egui::Context::default();
        context.enable_accesskit();
        frame(&context, &mut editor, vec![]);
        assert_eq!(editor.intent(), &intent);
        assert_eq!(
            serde_json::from_str::<GraphIntent>(&editor.raw).unwrap(),
            intent
        );
    }

    #[test]
    fn structured_format_and_processing_controls_preserve_typed_values() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut editor = GraphIntentEditor::default();
        let mut tree = frame(&context, &mut editor, vec![]);
        tree = set_value(&context, &mut editor, &tree, "in: Sample rate Hz", "88200");
        tree = set_value(
            &context,
            &mut editor,
            &tree,
            "in: Maximum quantum frames",
            "1024",
        );
        for (label, option) in [
            ("Requested processing precision", "Float64"),
            ("in: Sample domain", "DSD"),
            ("in: Output channels", "Mono"),
            ("in: Port precision", "Signed32"),
        ] {
            let id = named(&tree, label);
            let entry = tree
                .nodes
                .iter()
                .find(|(_, node)| node.labelled_by().contains(&id))
                .unwrap()
                .0;
            frame(
                &context,
                &mut editor,
                vec![egui::Event::AccessKitActionRequest(
                    egui::accesskit::ActionRequest {
                        action: egui::accesskit::Action::Click,
                        target_tree: tree.tree_id,
                        target_node: entry,
                        data: None,
                    },
                )],
            );
            tree = frame(&context, &mut editor, vec![]);
            click(&context, &mut editor, &tree, option);
            tree = frame(&context, &mut editor, vec![]);
        }
        let decoded: GraphIntent = serde_json::from_str(&editor.raw).unwrap();
        assert_eq!(
            decoded.graph.nodes[0].format,
            PortFormat {
                domain: SampleDomain::Dsd,
                sample_rate_hz: 88200,
                channels: ChannelLayout::Mono,
                precision: PcmRepresentation::Signed32,
                max_quantum_frames: 1024
            }
        );
        assert_eq!(
            decoded.processing,
            GraphProcessingRequest::Float64 {
                quantization: QuantizationPolicy::RoundTiesAwaySaturateNoDither
            }
        );
        assert_eq!(editor.intent(), &decoded);
    }

    #[test]
    fn dirty_raw_buffer_disables_chain_actions_and_structural_errors_keep_last_intent() {
        let mut editor = editor();
        let before = editor.intent().clone();
        editor.raw.push(' ');
        let context = egui::Context::default();
        context.enable_accesskit();
        let tree = frame(&context, &mut editor, vec![]);
        let id = named(&tree, "Add Gain");
        assert!(
            tree.nodes
                .iter()
                .find(|(node, _)| *node == id)
                .unwrap()
                .1
                .is_disabled()
        );
        click(&context, &mut editor, &tree, "Add Gain");
        assert_eq!(editor.intent(), &before);
        let mut value = serde_json::to_value(&before).unwrap();
        value["graph"]["nodes"][0]["kind"] =
            json!({"kind":"matrix","input_channels":"mono","coefficients":[1.0]});
        editor.raw = serde_json::to_string(&value).unwrap();
        assert!(!editor.accept_raw());
        assert_eq!(editor.intent(), &before);
        assert!(editor.error.is_some());
    }

    #[test]
    fn renderer_change_signal_requires_a_changed_canonical_intent() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut editor = editor();
        let mut value = serde_json::to_value(editor.intent()).unwrap();
        value["graph"]["schema_version"] = json!(2);
        editor.raw = serde_json::to_string(&value).unwrap();
        let tree = frame(&context, &mut editor, vec![]);
        let event = egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::Click,
            target_tree: tree.tree_id,
            target_node: named(&tree, "Accept raw edits"),
            data: None,
        });
        let mut changed = false;
        let mut output = context.run_ui(
            egui::RawInput {
                events: vec![event],
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1800.0, 8000.0),
                )),
                ..Default::default()
            },
            |ui| changed = editor.show(ui),
        );
        output.textures_delta.clear();
        assert!(changed);
        assert_eq!(editor.intent().graph.schema_version, 2);
        let mut output = context.run_ui(egui::RawInput::default(), |ui| changed = editor.show(ui));
        output.textures_delta.clear();
        assert!(!changed);
    }

    #[test]
    fn serial_convolution_reference_survives_rendering_without_asset_claims() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["graph"]["nodes"].as_array_mut().unwrap().insert(1,json!({"id":"fir","kind":{"kind":"convolution","asset":{"sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","sample_rate_hz":48000,"channels":"stereo","taps_per_channel":2048,"purpose":"correction"}},"format":format()}));
        value["graph"]["edges"] = json!([
            {"from":"in","to":"fir","from_port":0,"to_port":0},
            {"from":"fir","to":"out","from_port":0,"to_port":0}
        ]);
        let intent: GraphIntent = serde_json::from_value(value).unwrap();
        let mut editor = editor();
        editor.load(intent.clone());
        let context = egui::Context::default();
        context.enable_accesskit();
        let tree = frame(&context, &mut editor, vec![]);
        assert_eq!(editor.intent(), &intent);
        assert!(
            tree.nodes
                .iter()
                .filter_map(|(_, node)| node.label().or_else(|| node.value()))
                .any(|s| s.contains("Asset availability is unknown"))
        );
    }

    #[test]
    fn integer_controls_display_and_accept_values_without_float_rounding() {
        let mut intent = fixture();
        let graph = Arc::make_mut(&mut intent.graph);
        graph.nodes.insert(
            1,
            Node {
                id: NodeId::new("delay").unwrap(),
                kind: NodeKind::Delay {
                    delay_frames: 9007199254740993,
                },
                format: graph.nodes[0].format,
            },
        );
        graph.edges = vec![
            Edge {
                from: NodeId::new("in").unwrap(),
                to: NodeId::new("delay").unwrap(),
                from_port: 0,
                to_port: 0,
            },
            Edge {
                from: NodeId::new("delay").unwrap(),
                to: NodeId::new("out").unwrap(),
                from_port: 0,
                to_port: 0,
            },
        ];
        let mut editor = editor();
        editor.load(intent);
        let context = egui::Context::default();
        context.enable_accesskit();
        let tree = frame(&context, &mut editor, vec![]);
        let label = named(&tree, "delay: Delay frames");
        let entry = tree
            .nodes
            .iter()
            .find(|(_, node)| node.labelled_by().contains(&label))
            .unwrap();
        assert_eq!(entry.1.value(), Some("9007199254740993"));
        frame(
            &context,
            &mut editor,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::SetValue,
                    target_tree: tree.tree_id,
                    target_node: entry.0,
                    data: Some(egui::accesskit::ActionData::Value(
                        "9007199254740995".into(),
                    )),
                },
            )],
        );
        assert!(matches!(
            editor.intent().graph.nodes[1].kind,
            NodeKind::Delay {
                delay_frames: 9007199254740995
            }
        ));
        let tree = frame(&context, &mut editor, vec![]);
        let tree = set_value(&context, &mut editor, &tree, "delay: Delay frames", "-1");
        assert!(matches!(
            editor.intent().graph.nodes[1].kind,
            NodeKind::Delay {
                delay_frames: 9007199254740995
            }
        ));
        assert!(
            tree.nodes
                .iter()
                .filter_map(|(_, node)| node.label().or_else(|| node.value()))
                .any(|s| s.contains("previous value is retained"))
        );
    }

    #[test]
    fn src_preset_control_uses_the_typed_responsive_variant() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut editor = editor();
        let tree = frame(&context, &mut editor, vec![]);
        click(&context, &mut editor, &tree, "Add SRC");
        let tree = frame(&context, &mut editor, vec![]);
        let label = named(&tree, "node-1: SRC preset");
        let entry = tree
            .nodes
            .iter()
            .find(|(_, node)| node.labelled_by().contains(&label))
            .unwrap()
            .0;
        frame(
            &context,
            &mut editor,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Click,
                    target_tree: tree.tree_id,
                    target_node: entry,
                    data: None,
                },
            )],
        );
        let tree = frame(&context, &mut editor, vec![]);
        click(&context, &mut editor, &tree, "Responsive");
        assert!(matches!(
            editor.intent().graph.nodes[1].kind,
            NodeKind::Src {
                preset: SrcPreset::Responsive,
                ..
            }
        ));
        assert_eq!(
            serde_json::from_str::<GraphIntent>(&editor.raw).unwrap(),
            *editor.intent()
        );
    }

    #[test]
    fn finite_scalar_display_preserves_small_authored_values() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut editor = editor();
        let tree = frame(&context, &mut editor, vec![]);
        click(&context, &mut editor, &tree, "Add Gain");
        let node = &mut Arc::make_mut(&mut editor.intent.graph).nodes[1];
        node.kind = NodeKind::Gain {
            gain_linear: FiniteGraphScalar::new(1e-20).unwrap(),
        };
        editor.load(editor.intent().clone());
        let tree = frame(&context, &mut editor, vec![]);
        let label = named(&tree, "node-1: Linear gain");
        let entry = tree
            .nodes
            .iter()
            .find(|(_, node)| node.labelled_by().contains(&label))
            .unwrap();
        assert_eq!(entry.1.value().unwrap().parse::<f64>().unwrap(), 1e-20);
        assert!(
            matches!(editor.intent().graph.nodes[1].kind,NodeKind::Gain{gain_linear} if gain_linear.get()==1e-20)
        );
    }
    #[test]
    fn pending_raw_status_tracks_accept_and_restore_without_replacing_canonical_early() {
        let mut editor = GraphIntentEditor::default();
        assert!(!editor.has_pending_raw_edits());
        let original = editor.intent().clone();
        editor.raw = "{invalid raw draft".into();
        assert!(editor.has_pending_raw_edits());
        assert!(!editor.accept_raw());
        assert_eq!(editor.intent(), &original);
        assert!(editor.has_pending_raw_edits());
        editor.load(original.clone());
        assert!(!editor.has_pending_raw_edits());
        let mut revised = original.clone();
        Arc::make_mut(&mut revised.graph).nodes[0]
            .format
            .max_quantum_frames = 128;
        editor.raw = serde_json::to_string_pretty(&revised).unwrap();
        assert!(editor.has_pending_raw_edits());
        assert!(editor.accept_raw());
        assert!(!editor.has_pending_raw_edits());
        assert_eq!(editor.intent(), &revised);
    }
}
