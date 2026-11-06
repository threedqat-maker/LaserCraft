use std::collections::{HashMap, HashSet};

use egui::{Color32, CursorIcon, Painter, PointerButton, Pos2, Sense, Stroke};
use pf_core::{
    EllipseShape, Handle, LayerId, MarqueeMode, Path, Point, PolygonShape, Rect, RectShape,
    Segment, ShapeId, ShapeKind, SubPath, Transform, Vec2, Viewport, grid_step, handle_position,
    hit_handle, move_delta, pick_shape, rotate_delta, scale_delta, selection_bounds,
    shapes_in_rect,
};

use pf_gcode::GcodeError;

use crate::actions;
use crate::state::{
    AppState, CanvasTool, Interaction, NodeHandleRef, NodeHandleSide, NodeRef, SegmentRef,
    StatusLevel,
};

const FIT_MARGIN_PX: f64 = 24.0;
const MIN_GRID_PX: f64 = 10.0;
/// Every 10th grid line is drawn stronger.
const MAJOR_EVERY: i64 = 10;
/// Zoom factor per scrolled pixel; e^(0.002 · 50 px) ≈ 1.1 per wheel notch.
const SCROLL_ZOOM_RATE: f64 = 0.002;
const ORIGIN_MARK_PX: f32 = 12.0;

/// How close (px) the pointer must be to an outline to pick it.
const PICK_TOLERANCE_PX: f64 = 5.0;
const HANDLE_SIZE_PX: f32 = 7.0;
const HANDLE_HIT_PX: f64 = 6.0;
const LINE_CLOSE_SNAP_PX: f64 = 10.0;
/// Distance (px) of the rotate knob above the selection.
const ROTATE_OFFSET_PX: f64 = 22.0;
const NUDGE_MM: f64 = 1.0;
const NUDGE_FAST_MM: f64 = 10.0;
/// Rotation step while Shift is held.
const ROTATE_SNAP_DEG: f64 = 15.0;

const OUTSIDE_COLOR: Color32 = Color32::from_gray(70);
const BED_COLOR: Color32 = Color32::WHITE;
const BED_BORDER: Color32 = Color32::from_gray(40);
const GRID_MINOR: Color32 = Color32::from_gray(235);
const GRID_MAJOR: Color32 = Color32::from_gray(200);
const SELECTION_COLOR: Color32 = Color32::from_rgb(0, 120, 215);
const CROSSING_COLOR: Color32 = Color32::from_rgb(0, 150, 60);
const TRAVEL_COLOR: Color32 = Color32::from_gray(140);
const LASER_HEAD_COLOR: Color32 = Color32::from_rgb(230, 0, 120);
const LASER_HEAD_PX: f32 = 10.0;
const OUT_OF_BOUNDS_COLOR: Color32 = Color32::from_rgb(220, 30, 30);

pub fn show_canvas(ui: &mut egui::Ui, state: &mut AppState) {
    let response = ui.allocate_response(ui.available_size(), Sense::click_and_drag());
    let screen = to_rect(response.rect);

    if state.needs_fit {
        state.viewport = Viewport::fit(state.profile.work_area(), screen, FIT_MARGIN_PX);
        state.needs_fit = false;
    }
    let space_held = ui.input(|i| i.key_down(egui::Key::Space));
    handle_view_input(ui, &response, state, space_held);
    if !space_held {
        handle_tool_input(ui, &response, state);
    }
    handle_keys(ui, state);

    state.cursor_mm = response
        .hover_pos()
        .map(|pos| state.viewport.to_world(to_point(pos)));
    state
        .display
        .sync(&state.document, state.history.revision());
    if state.show_preview {
        actions::refresh_preview(state);
    }

    let pointer = ui
        .input(|i| i.pointer.latest_pos())
        .map(|pos| state.viewport.to_world(to_point(pos)));
    let shift = ui.input(|i| i.modifiers.shift);
    let preview = pointer.and_then(|p| drag_delta(&state.interaction, p, shift));
    if let Some(icon) = cursor_for(state, response.hover_pos().map(to_point)) {
        ui.ctx().set_cursor_icon(icon);
    }

    let painter = ui.painter_at(response.rect);
    painter.rect_filled(response.rect, 0.0, OUTSIDE_COLOR);
    draw_bed(&painter, state, screen);
    draw_shapes(&painter, state, screen, preview);
    draw_shape_preview(&painter, state, pointer, shift);
    draw_node_preview(&painter, state, pointer);
    draw_node_handles(&painter, state, pointer);
    if state.show_preview {
        draw_toolpaths(&painter, state);
    }
    draw_selection_frame(&painter, state, preview);
    draw_laser_head(&painter, state);
    if let (Interaction::Marquee { start }, Some(end)) = (state.interaction, pointer) {
        draw_marquee(&painter, &state.viewport, start, end);
    }
    if state.show_frame_time {
        draw_frame_time(ui, &painter, response.rect);
    }
}

fn handle_view_input(
    ui: &egui::Ui,
    response: &egui::Response,
    state: &mut AppState,
    space_held: bool,
) {
    if response.dragged_by(PointerButton::Middle)
        || (space_held && response.dragged_by(PointerButton::Primary))
    {
        let d = response.drag_delta();
        state
            .viewport
            .pan(Vec2::new(f64::from(d.x), f64::from(d.y)));
    }

    if let Some(anchor) = response.hover_pos() {
        let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let factor = (f64::from(scroll) * SCROLL_ZOOM_RATE).exp() * f64::from(pinch);
        if factor != 1.0 {
            state.viewport.zoom_about(to_point(anchor), factor);
        }
    }

    let no_text_focus = ui.memory(|m| m.focused().is_none());
    if no_text_focus && ui.input(|i| i.key_pressed(egui::Key::F) || i.key_pressed(egui::Key::Home))
    {
        state.needs_fit = true;
    }
}

fn handle_tool_input(ui: &egui::Ui, response: &egui::Response, state: &mut AppState) {
    let vp = state.viewport;
    let world = |pos: Pos2| vp.to_world(to_point(pos));
    let shift = ui.input(|i| i.modifiers.shift);

    if state.canvas_tool == CanvasTool::RoundCorners {
        if response.clicked_by(PointerButton::Primary)
            && let Some(pos) = response.interact_pointer_pos()
        {
            if let Some(node) = nearest_roundable_node(state, world(pos)) {
                if let Some(index) = state
                    .rounding_nodes
                    .iter()
                    .position(|selected| *selected == node)
                {
                    state.rounding_nodes.remove(index);
                } else {
                    state.rounding_nodes.push(node);
                }
                state.set_status(
                    StatusLevel::Info,
                    format!(
                        "{} corner(s) selected for rounding",
                        state.rounding_nodes.len()
                    ),
                );
            } else {
                state.set_status(
                    StatusLevel::Warning,
                    "Click a corner node on a selected straight-line shape",
                );
            }
        }
        return;
    }

    if state.canvas_tool == CanvasTool::Line {
        if response.clicked_by(PointerButton::Primary)
            && let Some(pos) = response.interact_pointer_pos()
        {
            let point = world(pos);
            if state.line_points.is_empty() {
                state.line_points.push(point);
            } else if should_close_line(state, point) {
                finish_line(state, true);
                state.interaction = Interaction::Idle;
                return;
            } else if let Some(&last) = state.line_points.last()
                && last.distance(point) > 0.0
            {
                state.line_points.push(constrain_line(last, point, shift));
            }
            if let Some(&last) = state.line_points.last() {
                state.interaction = Interaction::DrawShape {
                    tool: CanvasTool::Line,
                    start: last,
                };
            }
        }
        return;
    }

    if response.drag_started_by(PointerButton::Primary)
        && let Some(press) = ui.input(|i| i.pointer.press_origin())
    {
        state.interaction = match state.canvas_tool {
            CanvasTool::Select => begin_drag(state, world(press), shift),
            CanvasTool::EditNodes => begin_node_drag(state, world(press), shift),
            tool => Interaction::DrawShape {
                tool,
                start: world(press),
            },
        };
    }
    if response.drag_stopped_by(PointerButton::Primary) {
        match ui.input(|i| i.pointer.latest_pos()) {
            Some(end) => finish_drag(state, world(end), shift),
            None => state.interaction = Interaction::Idle,
        }
    }
    if response.clicked_by(PointerButton::Primary)
        && let Some(pos) = response.interact_pointer_pos()
    {
        match state.canvas_tool {
            CanvasTool::Select => click_select(state, world(pos), shift),
            CanvasTool::EditNodes => select_node_at(state, world(pos), shift),
            _ => {}
        }
    }
}

fn handle_keys(ui: &egui::Ui, state: &mut AppState) {
    if ui.memory(|m| m.focused().is_some()) {
        return;
    }
    if state.canvas_tool == CanvasTool::RoundCorners {
        let (escape, apply) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::Enter),
            )
        });
        if escape {
            state.rounding_nodes.clear();
            state.canvas_tool = CanvasTool::Select;
            state.set_status(StatusLevel::Info, "Corner rounding cancelled");
        } else if apply {
            crate::modifiers::apply_selected_corners(state);
            state.rounding_nodes.clear();
            state.canvas_tool = CanvasTool::Select;
        }
        return;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        if matches!(
            state.interaction,
            Interaction::DrawShape {
                tool: CanvasTool::Line,
                ..
            }
        ) {
            finish_line(state, false);
            state.interaction = Interaction::Idle;
            return;
        }
        if state.interaction == Interaction::Idle {
            state.selection.clear();
            state.selected_node = None;
            state.selected_nodes.clear();
            state.selected_segment = None;
            state.selected_segment_point = None;
        } else {
            state.interaction = Interaction::Idle;
        }
    }
    if state.interaction != Interaction::Idle {
        return;
    }
    if state.canvas_tool == CanvasTool::EditNodes {
        let action = ui.input(|input| {
            [
                (egui::Key::S, NodeEditAction::Smooth),
                (egui::Key::L, NodeEditAction::ToLine),
                (egui::Key::C, NodeEditAction::Corner),
                (egui::Key::D, NodeEditAction::Delete),
                (egui::Key::I, NodeEditAction::InsertAtPointer),
                (egui::Key::M, NodeEditAction::InsertMidpoint),
                (egui::Key::B, NodeEditAction::Break),
                (egui::Key::T, NodeEditAction::Trim),
                (egui::Key::E, NodeEditAction::Extend),
            ]
            .into_iter()
            .find_map(|(key, action)| input.key_pressed(key).then_some(action))
        });
        if let Some(action) = action {
            apply_node_edit_action(state, action);
            return;
        }
        if ui.input(|input| input.key_pressed(egui::Key::A)) {
            let points: Vec<Point> = state
                .selected_nodes
                .iter()
                .filter_map(|node| {
                    state.document.shape(node.shape_id).and_then(|shape| {
                        let path = editable_path(&shape.world_path());
                        path.subpaths()
                            .get(node.subpath)
                            .and_then(|subpath| anchor_point(subpath, node.anchor))
                    })
                })
                .collect();
            let horizontal = points.len() < 2 || {
                let min_x = points
                    .iter()
                    .map(|point| point.x)
                    .fold(f64::INFINITY, f64::min);
                let max_x = points
                    .iter()
                    .map(|point| point.x)
                    .fold(f64::NEG_INFINITY, f64::max);
                let min_y = points
                    .iter()
                    .map(|point| point.y)
                    .fold(f64::INFINITY, f64::min);
                let max_y = points
                    .iter()
                    .map(|point| point.y)
                    .fold(f64::NEG_INFINITY, f64::max);
                max_x - min_x >= max_y - min_y
            };
            align_selected_nodes(state, horizontal);
            return;
        }
    }
    if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
        actions::delete_selection(state);
    }
    let select_all = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::A);
    if ui.input_mut(|i| i.consume_shortcut(&select_all)) {
        actions::select_all(state);
    }

    let (step, arrows_down) = ui.input(|i| {
        let step = if i.modifiers.shift {
            NUDGE_FAST_MM
        } else {
            NUDGE_MM
        };
        let dx = f64::from(u8::from(i.key_pressed(egui::Key::ArrowRight)))
            - f64::from(u8::from(i.key_pressed(egui::Key::ArrowLeft)));
        let dy = f64::from(u8::from(i.key_pressed(egui::Key::ArrowUp)))
            - f64::from(u8::from(i.key_pressed(egui::Key::ArrowDown)));
        let down = [
            egui::Key::ArrowLeft,
            egui::Key::ArrowRight,
            egui::Key::ArrowUp,
            egui::Key::ArrowDown,
        ]
        .into_iter()
        .any(|k| i.key_down(k));
        (Vec2::new(dx * step, dy * step), down)
    });
    if step != Vec2::ZERO && !state.selection.is_empty() {
        actions::transform_selection(state, Transform::translate(step.x, step.y), true);
        state.nudging = true;
    } else if state.nudging && !arrows_down {
        // Releasing the keys ends the nudge, so the next one is a new step.
        state.history.seal();
        state.nudging = false;
    }
}

/// Decides what a primary drag starting at `press` (mm) does.
fn begin_drag(state: &mut AppState, press: Point, shift: bool) -> Interaction {
    let zoom = state.viewport.zoom();
    if let Some(bounds) = selection_bounds(&state.document, &state.selection) {
        let handle = hit_handle(bounds, press, HANDLE_HIT_PX / zoom, ROTATE_OFFSET_PX / zoom);
        match handle {
            Some(Handle::Rotate) => {
                return Interaction::Rotate {
                    start: press,
                    center: bounds.center(),
                };
            }
            Some(handle) => {
                return Interaction::Scale {
                    handle,
                    start: press,
                    bounds,
                };
            }
            None if bounds.contains_point(press) => {
                return Interaction::Move { start: press };
            }
            None => {}
        }
    }
    match pick_shape(&state.document, press, PICK_TOLERANCE_PX / zoom) {
        Some(id) => {
            if !state.selection.contains(&id) {
                if !shift {
                    state.selection.clear();
                }
                state.selection.push(id);
            }
            Interaction::Move { start: press }
        }
        None => Interaction::Marquee { start: press },
    }
}

fn begin_node_drag(state: &mut AppState, press: Point, shift: bool) -> Interaction {
    if let Some(node) = state.selected_node
        && let Some((handle, point, opposite, smooth)) =
            nearest_node_control_handle(state, node, press)
    {
        let anchor = selected_node_point(state, node).unwrap_or(point);
        return Interaction::MoveNodeHandle {
            handle: NodeHandleRef { node, side: handle },
            pointer_offset: Vec2::new(point.x - press.x, point.y - press.y),
            opposite_length: opposite.map(|point| point.distance(anchor)),
            smooth,
        };
    }
    let Some(node) = nearest_editable_node(state, press) else {
        if let Some(id) = pick_shape(
            &state.document,
            press,
            PICK_TOLERANCE_PX / state.viewport.zoom(),
        ) {
            state.selection = vec![id];
        }
        state.selected_node = None;
        state.selected_nodes.clear();
        state.selected_segment = nearest_editable_segment(state, press);
        state.selected_segment_point = state.selected_segment.map(|_| press);
        return Interaction::Idle;
    };
    if !shift {
        state.selection = vec![node.shape_id];
    } else if !state.selection.contains(&node.shape_id) {
        state.selection.push(node.shape_id);
    }
    if shift {
        if state
            .selected_nodes
            .first()
            .is_some_and(|selected| selected.shape_id != node.shape_id)
        {
            state.selected_nodes.clear();
        }
        if !state.selected_nodes.contains(&node) {
            state.selected_nodes.push(node);
        }
        state.selected_node = Some(node);
    } else if !state.selected_nodes.contains(&node) || state.selected_nodes.len() <= 1 {
        state.selected_nodes.clear();
        state.selected_nodes.push(node);
        state.selected_node = Some(node);
    }
    state.selected_segment = None;
    state.selected_segment_point = None;
    if state.selected_node.is_none() {
        state.selected_node = Some(node);
    }
    Interaction::MoveNode { node, start: press }
}

fn select_node_at(state: &mut AppState, at: Point, shift: bool) {
    if let Some(node) = nearest_editable_node(state, at) {
        if !shift {
            state.selection = vec![node.shape_id];
            state.selected_nodes.clear();
        } else if !state.selection.contains(&node.shape_id) {
            state.selection.push(node.shape_id);
        }
        if shift {
            if state
                .selected_nodes
                .first()
                .is_some_and(|selected| selected.shape_id != node.shape_id)
            {
                state.selected_nodes.clear();
            }
            if !state.selected_nodes.contains(&node) {
                state.selected_nodes.push(node);
            }
            state.selected_node = Some(node);
        } else {
            state.selected_nodes = vec![node];
            state.selected_node = Some(node);
        }
        state.selected_segment = None;
        state.selected_segment_point = None;
    } else {
        state.selected_node = None;
        state.selected_nodes.clear();
        if let Some(segment) = nearest_editable_segment(state, at) {
            if !shift || !state.selection.contains(&segment.shape_id) {
                state.selection = vec![segment.shape_id];
            }
            state.selected_segment = Some(segment);
            state.selected_segment_point = Some(at);
        } else if let Some(id) = pick_shape(
            &state.document,
            at,
            PICK_TOLERANCE_PX / state.viewport.zoom(),
        ) {
            state.selection = vec![id];
            state.selected_segment = None;
            state.selected_segment_point = None;
        } else {
            state.selection.clear();
            state.selected_segment = None;
            state.selected_segment_point = None;
        }
    }
}

fn nearest_editable_segment(state: &AppState, at: Point) -> Option<SegmentRef> {
    let tolerance = PICK_TOLERANCE_PX / state.viewport.zoom();
    let selected: HashSet<ShapeId> = state.selection.iter().copied().collect();
    let mut closest: Option<(SegmentRef, f64)> = None;
    for shape in state
        .document
        .shapes()
        .iter()
        .filter(|shape| selected.is_empty() || selected.contains(&shape.id))
    {
        let path = editable_path(&shape.world_path());
        for (subpath_index, subpath) in path.subpaths().iter().enumerate() {
            for (segment_index, segment) in subpath.segments().iter().enumerate() {
                let distance = (0..=32)
                    .map(|step| segment.point_at(f64::from(step) / 32.0).distance(at))
                    .fold(f64::INFINITY, f64::min);
                if distance <= tolerance && closest.map_or(true, |(_, best)| distance < best) {
                    closest = Some((
                        SegmentRef {
                            shape_id: shape.id,
                            subpath: subpath_index,
                            segment: segment_index,
                        },
                        distance,
                    ));
                }
            }
        }
    }
    closest.map(|(segment, _)| segment)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeEditAction {
    Smooth,
    ToLine,
    Corner,
    Delete,
    InsertAtPointer,
    InsertMidpoint,
    Break,
    Trim,
    Extend,
    AlignHorizontal,
    AlignVertical,
}

/// Context menu shared by the Edit Nodes tool and the Edit menu.
pub fn show_node_editor_menu(ui: &mut egui::Ui, state: &mut AppState) {
    ui.label("Edit Nodes");
    ui.separator();
    let node_selected = state.selected_node.is_some();
    let segment_selected = state.selected_segment.is_some();
    let insert_target = segment_selected
        || state
            .cursor_mm
            .is_some_and(|point| nearest_editable_segment(state, point).is_some());
    for (label, action, enabled) in [
        (
            "Smooth node or line (S)",
            NodeEditAction::Smooth,
            node_selected || segment_selected,
        ),
        (
            "Convert curve to line (L)",
            NodeEditAction::ToLine,
            segment_selected,
        ),
        (
            "Convert node to corner (C)",
            NodeEditAction::Corner,
            node_selected,
        ),
        (
            "Delete node or line (D)",
            NodeEditAction::Delete,
            node_selected || segment_selected,
        ),
        (
            "Insert node point (I)",
            NodeEditAction::InsertAtPointer,
            insert_target,
        ),
        (
            "Insert node at midpoint (M)",
            NodeEditAction::InsertMidpoint,
            segment_selected,
        ),
        (
            "Break shape at point (B)",
            NodeEditAction::Break,
            node_selected,
        ),
        ("Trim line (T)", NodeEditAction::Trim, segment_selected),
        ("Extend line (E)", NodeEditAction::Extend, segment_selected),
    ] {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            apply_node_edit_action(state, action);
            ui.close();
        }
    }
    ui.add_enabled_ui(state.selected_nodes.len() > 1, |ui| {
        ui.menu_button("Align selected (A)", |ui| {
            if ui.button("Align horizontally").clicked() {
                apply_node_edit_action(state, NodeEditAction::AlignHorizontal);
                ui.close();
            }
            if ui.button("Align vertically").clicked() {
                apply_node_edit_action(state, NodeEditAction::AlignVertical);
                ui.close();
            }
        })
    });
}

fn apply_node_edit_action(state: &mut AppState, action: NodeEditAction) {
    let segment_ref = if matches!(action, NodeEditAction::InsertAtPointer) {
        state
            .cursor_mm
            .and_then(|point| nearest_editable_segment(state, point))
            .or(state.selected_segment)
    } else {
        state.selected_segment
    };
    match action {
        NodeEditAction::InsertAtPointer | NodeEditAction::InsertMidpoint => {
            let Some(segment_ref) = segment_ref else {
                return;
            };
            let Some(shape) = state.document.shape(segment_ref.shape_id) else {
                return;
            };
            let path = editable_path(&shape.world_path());
            let Some(segment) = path
                .subpaths()
                .get(segment_ref.subpath)
                .and_then(|subpath| subpath.segments().get(segment_ref.segment))
                .copied()
            else {
                return;
            };
            let t = if action == NodeEditAction::InsertMidpoint {
                0.5
            } else {
                state
                    .cursor_mm
                    .or(state.selected_segment_point)
                    .map_or(0.5, |point| nearest_parameter(segment, point))
            };
            if let Some(path) = insert_path_node(&path, segment_ref.subpath, segment_ref.segment, t)
            {
                let inserted_anchor = if path.subpaths()[segment_ref.subpath].is_closed()
                    && segment_ref.segment + 1
                        == path.subpaths()[segment_ref.subpath].segments().len()
                {
                    0
                } else {
                    segment_ref.segment + 1
                };
                actions::set_shape_geometry(
                    state,
                    segment_ref.shape_id,
                    ShapeKind::Path(path),
                    Transform::identity(),
                );
                state.selected_node = Some(NodeRef {
                    shape_id: segment_ref.shape_id,
                    subpath: segment_ref.subpath,
                    anchor: inserted_anchor,
                });
                state.selected_nodes = state.selected_node.into_iter().collect();
                state.selected_segment = None;
                state.selected_segment_point = None;
            }
        }
        NodeEditAction::ToLine | NodeEditAction::Trim | NodeEditAction::Extend => {
            let Some(segment_ref) = segment_ref else {
                return;
            };
            edit_selected_segment(state, segment_ref, action);
        }
        NodeEditAction::Delete => {
            if let Some(node) = state.selected_node {
                delete_path_node(state, node);
            } else if let Some(segment) = segment_ref {
                delete_path_segment(state, segment);
            }
        }
        NodeEditAction::Smooth | NodeEditAction::Corner | NodeEditAction::Break => {
            if let Some(node) = state.selected_node {
                edit_selected_node(state, node, action);
            } else if action == NodeEditAction::Smooth
                && let Some(segment) = segment_ref
            {
                edit_selected_segment(state, segment, NodeEditAction::Smooth);
            }
        }
        NodeEditAction::AlignHorizontal | NodeEditAction::AlignVertical => {
            align_selected_nodes(state, action == NodeEditAction::AlignHorizontal);
        }
    }
}

fn nearest_parameter(segment: Segment, point: Point) -> f64 {
    let coarse = (0..=100)
        .map(|step| f64::from(step) / 100.0)
        .min_by(|a, b| {
            segment
                .point_at(*a)
                .distance(point)
                .total_cmp(&segment.point_at(*b).distance(point))
        })
        .unwrap_or(0.5);
    let mut low = (coarse - 0.01).max(0.0);
    let mut high = (coarse + 0.01).min(1.0);
    for _ in 0..20 {
        let left = low + (high - low) / 3.0;
        let right = high - (high - low) / 3.0;
        if segment.point_at(left).distance(point) <= segment.point_at(right).distance(point) {
            high = right;
        } else {
            low = left;
        }
    }
    (low + high) / 2.0
}

fn split_segment(segment: Segment, t: f64) -> (Segment, Segment) {
    match segment {
        Segment::Line { from, to } => {
            let mid = from.lerp(to, t);
            (
                Segment::Line { from, to: mid },
                Segment::Line { from: mid, to },
            )
        }
        Segment::QuadBezier { from, ctrl, to } => {
            let a = from.lerp(ctrl, t);
            let b = ctrl.lerp(to, t);
            let mid = a.lerp(b, t);
            (
                Segment::QuadBezier {
                    from,
                    ctrl: a,
                    to: mid,
                },
                Segment::QuadBezier {
                    from: mid,
                    ctrl: b,
                    to,
                },
            )
        }
        Segment::CubicBezier {
            from,
            ctrl1,
            ctrl2,
            to,
        } => {
            let a = from.lerp(ctrl1, t);
            let b = ctrl1.lerp(ctrl2, t);
            let c = ctrl2.lerp(to, t);
            let d = a.lerp(b, t);
            let e = b.lerp(c, t);
            let mid = d.lerp(e, t);
            (
                Segment::CubicBezier {
                    from,
                    ctrl1: a,
                    ctrl2: d,
                    to: mid,
                },
                Segment::CubicBezier {
                    from: mid,
                    ctrl1: e,
                    ctrl2: c,
                    to,
                },
            )
        }
        Segment::Arc {
            center,
            radii,
            x_rotation,
            start_angle,
            sweep_angle,
        } => (
            Segment::Arc {
                center,
                radii,
                x_rotation,
                start_angle,
                sweep_angle: sweep_angle * t,
            },
            Segment::Arc {
                center,
                radii,
                x_rotation,
                start_angle: start_angle + sweep_angle * t,
                sweep_angle: sweep_angle * (1.0 - t),
            },
        ),
    }
}

fn insert_path_node(
    path: &Path,
    subpath_index: usize,
    segment_index: usize,
    t: f64,
) -> Option<Path> {
    let mut subpaths = path.subpaths().to_vec();
    let subpath = subpaths.get_mut(subpath_index)?;
    let mut segments = subpath.segments().to_vec();
    let (first, second) = split_segment(*segments.get(segment_index)?, t.clamp(1e-6, 1.0 - 1e-6));
    segments[segment_index] = first;
    segments.insert(segment_index + 1, second);
    if subpath.is_closed() && segment_index + 1 == segments.len() - 1 {
        segments.rotate_left(segment_index + 1);
    }
    *subpath = SubPath::new(segments, subpath.is_closed()).ok()?;
    Some(Path::new(subpaths))
}

fn as_cubic(segment: Segment) -> Segment {
    match segment {
        Segment::Line { from, to } => Segment::CubicBezier {
            from,
            ctrl1: from.lerp(to, 1.0 / 3.0),
            ctrl2: from.lerp(to, 2.0 / 3.0),
            to,
        },
        Segment::QuadBezier { from, ctrl, to } => Segment::CubicBezier {
            from,
            ctrl1: from.lerp(ctrl, 2.0 / 3.0),
            ctrl2: to.lerp(ctrl, 2.0 / 3.0),
            to,
        },
        other => other,
    }
}

fn edit_selected_segment(state: &mut AppState, reference: SegmentRef, action: NodeEditAction) {
    let Some(shape) = state.document.shape(reference.shape_id) else {
        return;
    };
    let mut path = editable_path(&shape.world_path());
    let Some(subpath) = path.subpaths().get(reference.subpath) else {
        return;
    };
    let Some(segment) = subpath.segments().get(reference.segment).copied() else {
        return;
    };
    let replacement = match action {
        NodeEditAction::Smooth => Some(as_cubic(segment)),
        NodeEditAction::ToLine => Some(Segment::Line {
            from: segment.start(),
            to: segment.end(),
        }),
        NodeEditAction::Trim | NodeEditAction::Extend => {
            if !matches!(segment, Segment::Line { .. }) {
                return;
            }
            let Some(pointer) = state.cursor_mm.or(state.selected_segment_point) else {
                return;
            };
            let trim_start = pointer.distance(segment.start()) < pointer.distance(segment.end());
            let intersections = line_intersections(state, reference, segment);
            let parameter = if action == NodeEditAction::Trim {
                if trim_start {
                    intersections
                        .into_iter()
                        .filter(|t| *t > 0.0 && *t < 1.0)
                        .min_by(f64::total_cmp)
                } else {
                    intersections
                        .into_iter()
                        .filter(|t| *t > 0.0 && *t < 1.0)
                        .max_by(f64::total_cmp)
                }
            } else if trim_start {
                intersections
                    .into_iter()
                    .filter(|t| *t < 0.0)
                    .max_by(f64::total_cmp)
            } else {
                intersections
                    .into_iter()
                    .filter(|t| *t > 1.0)
                    .min_by(f64::total_cmp)
            };
            let Some(t) = parameter else {
                state.set_status(
                    crate::state::StatusLevel::Info,
                    "No line intersection found",
                );
                return;
            };
            let new_anchor = if trim_start {
                reference.segment
            } else {
                reference.segment + 1
            };
            let point = segment.point_at(t);
            move_path_anchor(
                &path,
                reference.subpath,
                new_anchor,
                Vec2::new(
                    point.x
                        - if trim_start {
                            segment.start().x
                        } else {
                            segment.end().x
                        },
                    point.y
                        - if trim_start {
                            segment.start().y
                        } else {
                            segment.end().y
                        },
                ),
            )
            .map(|updated| {
                path = updated;
                segment
            })
        }
        _ => None,
    };
    if action == NodeEditAction::ToLine || action == NodeEditAction::Smooth {
        let mut subpaths = path.subpaths().to_vec();
        if let Some(subpath) = subpaths.get_mut(reference.subpath) {
            let mut segments = subpath.segments().to_vec();
            if reference.segment < segments.len() {
                segments[reference.segment] = replacement.unwrap();
                if let Ok(updated) = SubPath::new(segments, subpath.is_closed()) {
                    *subpath = updated;
                }
            }
        }
        path = Path::new(subpaths);
    } else if replacement.is_none() {
        return;
    }
    actions::set_shape_geometry(
        state,
        reference.shape_id,
        ShapeKind::Path(path),
        Transform::identity(),
    );
}

fn line_intersections(state: &AppState, target_ref: SegmentRef, target: Segment) -> Vec<f64> {
    let Segment::Line { from, to } = target else {
        return Vec::new();
    };
    let r = Vec2::new(to.x - from.x, to.y - from.y);
    let mut intersections = Vec::new();
    for shape in state.document.shapes() {
        let path = editable_path(&shape.world_path());
        for (subpath_index, subpath) in path.subpaths().iter().enumerate() {
            for (segment_index, segment) in subpath.segments().iter().enumerate() {
                if shape.id == target_ref.shape_id
                    && subpath_index == target_ref.subpath
                    && segment_index == target_ref.segment
                {
                    continue;
                }
                let samples = match segment {
                    Segment::Line { .. } => 1,
                    _ => 24,
                };
                for sample in 0..samples {
                    let a = segment.point_at(f64::from(sample) / f64::from(samples));
                    let b = segment.point_at(f64::from(sample + 1) / f64::from(samples));
                    let s = Vec2::new(b.x - a.x, b.y - a.y);
                    let denom = cross(r, s);
                    if denom.abs() < 1e-12 {
                        continue;
                    }
                    let q = Vec2::new(a.x - from.x, a.y - from.y);
                    let t = cross(q, s) / denom;
                    let u = cross(q, r) / denom;
                    if (-1e-9..=1.0 + 1e-9).contains(&u)
                        && !intersections
                            .iter()
                            .any(|found: &f64| (*found - t).abs() < 1e-7)
                    {
                        intersections.push(t);
                    }
                }
            }
        }
    }
    intersections
}

fn cross(a: Vec2, b: Vec2) -> f64 {
    a.x * b.y - a.y * b.x
}

fn edit_selected_node(state: &mut AppState, node: NodeRef, action: NodeEditAction) {
    let Some(shape) = state.document.shape(node.shape_id) else {
        return;
    };
    let mut path = editable_path(&shape.world_path());
    let Some(subpath) = path.subpaths().get(node.subpath) else {
        return;
    };
    let closed = subpath.is_closed();
    let segments = subpath.segments().to_vec();
    let count = segments.len();
    if count == 0 {
        return;
    }
    let anchor_count = count + if closed { 0 } else { 1 };
    if node.anchor >= anchor_count {
        return;
    }
    let current = (node.anchor < count).then_some(node.anchor);
    let previous = if node.anchor > 0 {
        Some(node.anchor - 1)
    } else if closed {
        Some(count - 1)
    } else {
        None
    };
    let mut changed = false;
    match action {
        NodeEditAction::Smooth | NodeEditAction::Corner => {
            let anchor = current
                .map(|index| segments[index].start())
                .or_else(|| previous.map(|index| segments[index].end()))
                .unwrap();
            let mut updated = segments;
            if let Some(index) = previous {
                let Segment::CubicBezier {
                    from,
                    ctrl1,
                    ctrl2: _,
                    to,
                } = as_cubic(updated[index])
                else {
                    return;
                };
                if action == NodeEditAction::Corner {
                    updated[index] = Segment::CubicBezier {
                        from,
                        ctrl1,
                        ctrl2: anchor,
                        to,
                    };
                } else if let Some(next) = current {
                    let Segment::CubicBezier {
                        from: next_from,
                        ctrl1: _,
                        ctrl2,
                        to: next_to,
                    } = as_cubic(updated[next])
                    else {
                        return;
                    };
                    let tangent = Vec2::new(next_to.x - from.x, next_to.y - from.y);
                    let length = tangent.x.hypot(tangent.y);
                    if length <= 1e-12 {
                        return;
                    }
                    let direction = Vec2::new(tangent.x / length, tangent.y / length);
                    let incoming = from.distance(anchor) / 3.0;
                    let outgoing = anchor.distance(next_to) / 3.0;
                    updated[index] = Segment::CubicBezier {
                        from,
                        ctrl1,
                        ctrl2: Point::new(
                            anchor.x - direction.x * incoming,
                            anchor.y - direction.y * incoming,
                        ),
                        to,
                    };
                    updated[next] = Segment::CubicBezier {
                        from: next_from,
                        ctrl1: Point::new(
                            anchor.x + direction.x * outgoing,
                            anchor.y + direction.y * outgoing,
                        ),
                        ctrl2,
                        to: next_to,
                    };
                } else {
                    updated[index] = as_cubic(updated[index]);
                }
            } else if action == NodeEditAction::Smooth
                && let Some(index) = current
            {
                updated[index] = as_cubic(updated[index]);
            }
            if let Some(index) = current
                && action == NodeEditAction::Corner
            {
                if let Segment::CubicBezier {
                    from,
                    ctrl1: _,
                    ctrl2,
                    to,
                } = as_cubic(updated[index])
                {
                    updated[index] = Segment::CubicBezier {
                        from,
                        ctrl1: anchor,
                        ctrl2,
                        to,
                    };
                }
            }
            if let Ok(subpath) = SubPath::new(updated, closed) {
                let mut subpaths = path.subpaths().to_vec();
                subpaths[node.subpath] = subpath;
                path = Path::new(subpaths);
                changed = true;
            }
        }
        NodeEditAction::Break => {
            if closed {
                if let Ok(open) = SubPath::new(segments, false) {
                    let mut subpaths = path.subpaths().to_vec();
                    subpaths[node.subpath] = open;
                    path = Path::new(subpaths);
                    changed = true;
                }
            } else if node.anchor > 0 && node.anchor < count {
                let left = SubPath::new(segments[..node.anchor].to_vec(), false).ok();
                let right = SubPath::new(segments[node.anchor..].to_vec(), false).ok();
                if let (Some(left), Some(right)) = (left, right) {
                    let mut subpaths = path.subpaths().to_vec();
                    subpaths.splice(node.subpath..=node.subpath, [left, right]);
                    path = Path::new(subpaths);
                    changed = true;
                }
            }
        }
        _ => return,
    }
    if changed {
        actions::set_shape_geometry(
            state,
            node.shape_id,
            ShapeKind::Path(path),
            Transform::identity(),
        );
        state.selected_node = None;
        state.selected_nodes.clear();
    }
}

fn delete_path_node(state: &mut AppState, node: NodeRef) {
    let Some(shape) = state.document.shape(node.shape_id) else {
        return;
    };
    let path = editable_path(&shape.world_path());
    let Some(subpath) = path.subpaths().get(node.subpath) else {
        return;
    };
    let mut segments = subpath.segments().to_vec();
    let count = segments.len();
    let anchor_count = count + if subpath.is_closed() { 0 } else { 1 };
    if node.anchor >= anchor_count || count == 0 {
        return;
    }
    if count == 1 && !subpath.is_closed() {
        delete_path_segment(
            state,
            SegmentRef {
                shape_id: node.shape_id,
                subpath: node.subpath,
                segment: 0,
            },
        );
        return;
    }
    if count < 2 {
        return;
    }
    if subpath.is_closed() {
        let previous = (node.anchor + count - 1) % count;
        let current = node.anchor % count;
        let from = segments[previous].start();
        let to = segments[current].end();
        let prev = as_cubic(segments[previous]);
        let next = as_cubic(segments[current]);
        let (ctrl1, ctrl2) = match (prev, next) {
            (Segment::CubicBezier { ctrl1, .. }, Segment::CubicBezier { ctrl2, .. }) => {
                (ctrl1, ctrl2)
            }
            _ => (from.lerp(to, 1.0 / 3.0), from.lerp(to, 2.0 / 3.0)),
        };
        let merged = Segment::CubicBezier {
            from,
            ctrl1,
            ctrl2,
            to,
        };
        if node.anchor == 0 {
            segments[count - 1] = merged;
            segments.remove(0);
        } else {
            segments[current] = merged;
            segments.remove(previous);
        }
        if let Ok(updated) = SubPath::new(segments, true) {
            let mut subpaths = path.subpaths().to_vec();
            subpaths[node.subpath] = updated;
            actions::set_shape_geometry(
                state,
                node.shape_id,
                ShapeKind::Path(Path::new(subpaths)),
                Transform::identity(),
            );
        }
    } else if node.anchor == 0 || node.anchor == count {
        segments.remove(if node.anchor == 0 { 0 } else { count - 1 });
        if let Ok(updated) = SubPath::new(segments, false) {
            let mut subpaths = path.subpaths().to_vec();
            subpaths[node.subpath] = updated;
            actions::set_shape_geometry(
                state,
                node.shape_id,
                ShapeKind::Path(Path::new(subpaths)),
                Transform::identity(),
            );
        }
    } else {
        let previous = segments[node.anchor - 1];
        let next = segments[node.anchor];
        let from = previous.start();
        let to = next.end();
        let (ctrl1, ctrl2) = match (as_cubic(previous), as_cubic(next)) {
            (Segment::CubicBezier { ctrl1, .. }, Segment::CubicBezier { ctrl2, .. }) => {
                (ctrl1, ctrl2)
            }
            _ => (from.lerp(to, 1.0 / 3.0), from.lerp(to, 2.0 / 3.0)),
        };
        segments[node.anchor - 1] = Segment::CubicBezier {
            from,
            ctrl1,
            ctrl2,
            to,
        };
        segments.remove(node.anchor);
        if let Ok(updated) = SubPath::new(segments, false) {
            let mut subpaths = path.subpaths().to_vec();
            subpaths[node.subpath] = updated;
            actions::set_shape_geometry(
                state,
                node.shape_id,
                ShapeKind::Path(Path::new(subpaths)),
                Transform::identity(),
            );
        }
    }
    state.selected_node = None;
    state.selected_nodes.clear();
}

fn delete_path_segment(state: &mut AppState, reference: SegmentRef) {
    let Some(shape) = state.document.shape(reference.shape_id) else {
        return;
    };
    let path = editable_path(&shape.world_path());
    let Some(subpath) = path.subpaths().get(reference.subpath) else {
        return;
    };
    let segments = subpath.segments();
    if reference.segment >= segments.len() {
        return;
    }
    let mut subpaths = path.subpaths().to_vec();
    if subpath.is_closed() {
        let mut remaining = segments[reference.segment + 1..].to_vec();
        remaining.extend_from_slice(&segments[..reference.segment]);
        if remaining.is_empty() {
            subpaths.remove(reference.subpath);
        } else {
            let Ok(open) = SubPath::new(remaining, false) else {
                return;
            };
            subpaths[reference.subpath] = open;
        }
    } else {
        let left = SubPath::new(segments[..reference.segment].to_vec(), false).ok();
        let right = SubPath::new(segments[reference.segment + 1..].to_vec(), false).ok();
        let mut replacements = Vec::new();
        replacements.extend(left);
        replacements.extend(right);
        subpaths.splice(reference.subpath..=reference.subpath, replacements);
    }
    if subpaths.is_empty() {
        actions::delete_shape(state, reference.shape_id);
    } else {
        actions::set_shape_geometry(
            state,
            reference.shape_id,
            ShapeKind::Path(Path::new(subpaths)),
            Transform::identity(),
        );
    }
    state.selected_node = None;
    state.selected_nodes.clear();
    state.selected_segment = None;
    state.selected_segment_point = None;
}

fn align_selected_nodes(state: &mut AppState, horizontal: bool) {
    let mut nodes = state.selected_nodes.clone();
    if let Some(active) = state.selected_node
        && !nodes.contains(&active)
    {
        nodes.push(active);
    }
    if nodes.len() < 2 {
        return;
    }
    let Some(primary) = state.selected_node.or(nodes.first().copied()) else {
        return;
    };
    let Some(shape) = state.document.shape(primary.shape_id) else {
        return;
    };
    let path = editable_path(&shape.world_path());
    let Some(subpath) = path.subpaths().get(primary.subpath) else {
        return;
    };
    let Some(anchor) = anchor_point(subpath, primary.anchor) else {
        return;
    };
    let shape_ids: HashSet<ShapeId> = nodes.iter().map(|node| node.shape_id).collect();
    for shape_id in shape_ids {
        let Some(shape) = state.document.shape(shape_id) else {
            continue;
        };
        let mut updated = editable_path(&shape.world_path());
        for node in nodes.iter().filter(|node| node.shape_id == shape_id) {
            let Some(current_subpath) = updated.subpaths().get(node.subpath) else {
                continue;
            };
            let Some(current) = anchor_point(current_subpath, node.anchor) else {
                continue;
            };
            let target = if horizontal {
                Point::new(current.x, anchor.y)
            } else {
                Point::new(anchor.x, current.y)
            };
            if let Some(path) = move_path_anchor(
                &updated,
                node.subpath,
                node.anchor,
                Vec2::new(target.x - current.x, target.y - current.y),
            ) {
                updated = path;
            }
        }
        actions::set_shape_geometry(
            state,
            shape_id,
            ShapeKind::Path(updated),
            Transform::identity(),
        );
    }
}

fn anchor_point(subpath: &SubPath, anchor: usize) -> Option<Point> {
    let count = subpath.segments().len();
    if anchor < count {
        Some(subpath.segments()[anchor].start())
    } else if !subpath.is_closed() && anchor == count {
        Some(subpath.end())
    } else {
        None
    }
}

fn nearest_editable_node(state: &AppState, at: Point) -> Option<NodeRef> {
    let tolerance = HANDLE_HIT_PX / state.viewport.zoom();
    let selected: HashSet<ShapeId> = state.selection.iter().copied().collect();
    let mut closest: Option<(NodeRef, f64)> = None;
    for shape in state
        .document
        .shapes()
        .iter()
        .filter(|shape| selected.is_empty() || selected.contains(&shape.id))
    {
        let path = editable_path(&shape.world_path());
        for (subpath_index, subpath) in path.subpaths().iter().enumerate() {
            for (anchor, point) in editable_anchors(subpath) {
                let distance = point.distance(at);
                if distance <= tolerance
                    && closest.map_or(true, |(_, closest_distance)| distance < closest_distance)
                {
                    closest = Some((
                        NodeRef {
                            shape_id: shape.id,
                            subpath: subpath_index,
                            anchor,
                        },
                        distance,
                    ));
                }
            }
        }
    }
    closest.map(|(node, _)| node)
}

fn nearest_roundable_node(state: &AppState, at: Point) -> Option<NodeRef> {
    let tolerance = HANDLE_HIT_PX / state.viewport.zoom();
    let selected: HashSet<ShapeId> = state.selection.iter().copied().collect();
    let mut closest: Option<(NodeRef, f64)> = None;
    for shape in state
        .document
        .shapes()
        .iter()
        .filter(|shape| selected.contains(&shape.id))
    {
        let path = editable_path(&shape.world_path());
        for (subpath_index, subpath) in path.subpaths().iter().enumerate() {
            for anchor in roundable_anchors(subpath) {
                let Some(point) = anchor_point(subpath, anchor) else {
                    continue;
                };
                let distance = point.distance(at);
                if distance <= tolerance && closest.map_or(true, |(_, d)| distance < d) {
                    closest = Some((
                        NodeRef {
                            shape_id: shape.id,
                            subpath: subpath_index,
                            anchor,
                        },
                        distance,
                    ));
                }
            }
        }
    }
    closest.map(|(node, _)| node)
}

fn roundable_anchors(subpath: &SubPath) -> Vec<usize> {
    let count = subpath.segments().len();
    let anchors: Box<dyn Iterator<Item = usize>> = if subpath.is_closed() {
        Box::new(0..count)
    } else {
        Box::new(1..count)
    };
    anchors
        .filter(|&anchor| {
            let previous = if anchor == 0 {
                count.checked_sub(1)
            } else {
                Some(anchor - 1)
            };
            let next = (anchor < count).then_some(anchor);
            let Some((previous, next)) = previous.zip(next) else {
                return false;
            };
            let (
                Segment::Line { from: p0, to: p1 },
                Segment::Line {
                    from: p1_next,
                    to: p2,
                },
            ) = (subpath.segments()[previous], subpath.segments()[next])
            else {
                return false;
            };
            if p1.distance(p1_next) > 1e-6 {
                return false;
            }
            let Some(incoming) = (p1 - p0).normalize() else {
                return false;
            };
            let Some(outgoing) = (p2 - p1_next).normalize() else {
                return false;
            };
            incoming.cross(outgoing).abs() > 1e-4 && incoming.dot(outgoing) > -0.9999
        })
        .collect()
}

fn selected_node_point(state: &AppState, node: NodeRef) -> Option<Point> {
    let shape = state.document.shape(node.shape_id)?;
    let path = editable_path(&shape.world_path());
    let subpath = path.subpaths().get(node.subpath)?;
    anchor_point(subpath, node.anchor)
}

fn node_control_points(
    path: &Path,
    node: NodeRef,
) -> Option<(Point, Option<Point>, Option<Point>)> {
    let subpath = path.subpaths().get(node.subpath)?;
    let segments = subpath.segments();
    let count = segments.len();
    if count == 0 {
        return None;
    }
    let anchor = anchor_point(subpath, node.anchor)?;
    let current = (node.anchor < count).then_some(node.anchor);
    let previous = if node.anchor > 0 {
        Some(node.anchor - 1)
    } else if subpath.is_closed() {
        Some(count - 1)
    } else {
        None
    };
    let incoming = previous.and_then(|index| match segments[index] {
        Segment::CubicBezier { ctrl2, .. } => Some(ctrl2),
        _ => None,
    });
    let outgoing = current.and_then(|index| match segments[index] {
        Segment::CubicBezier { ctrl1, .. } => Some(ctrl1),
        _ => None,
    });
    Some((anchor, incoming, outgoing))
}

fn node_is_smooth(anchor: Point, incoming: Option<Point>, outgoing: Option<Point>) -> bool {
    let (Some(incoming), Some(outgoing)) = (incoming, outgoing) else {
        return false;
    };
    let a = Vec2::new(anchor.x - incoming.x, anchor.y - incoming.y);
    let b = Vec2::new(outgoing.x - anchor.x, outgoing.y - anchor.y);
    let lengths = a.x.hypot(a.y) * b.x.hypot(b.y);
    lengths > 1e-12 && cross(a, b).abs() <= lengths * 1e-4 && a.x * b.x + a.y * b.y > 0.0
}

fn move_path_control(
    path: &Path,
    node: NodeRef,
    side: NodeHandleSide,
    target: Point,
    opposite_length: Option<f64>,
    linked: bool,
) -> Option<Path> {
    let mut subpaths = path.subpaths().to_vec();
    let subpath = subpaths.get_mut(node.subpath)?;
    let (anchor, _, _) = node_control_points(path, node)?;
    let mut segments = subpath.segments().to_vec();
    let count = segments.len();
    let current = (node.anchor < count).then_some(node.anchor);
    let previous = if node.anchor > 0 {
        Some(node.anchor - 1)
    } else if subpath.is_closed() {
        Some(count - 1)
    } else {
        None
    };
    match side {
        NodeHandleSide::Incoming => {
            let index = previous?;
            let Segment::CubicBezier { ctrl2, .. } = &mut segments[index] else {
                return None;
            };
            *ctrl2 = target;
        }
        NodeHandleSide::Outgoing => {
            let index = current?;
            let Segment::CubicBezier { ctrl1, .. } = &mut segments[index] else {
                return None;
            };
            *ctrl1 = target;
        }
    }
    if linked && let Some(length) = opposite_length {
        let vector = Vec2::new(target.x - anchor.x, target.y - anchor.y);
        let magnitude = vector.x.hypot(vector.y);
        let opposite = if magnitude > 1e-12 {
            Point::new(
                anchor.x - vector.x / magnitude * length,
                anchor.y - vector.y / magnitude * length,
            )
        } else {
            anchor
        };
        match side {
            NodeHandleSide::Incoming => {
                if let Some(index) = current
                    && let Segment::CubicBezier { ctrl1, .. } = &mut segments[index]
                {
                    *ctrl1 = opposite;
                }
            }
            NodeHandleSide::Outgoing => {
                if let Some(index) = previous
                    && let Segment::CubicBezier { ctrl2, .. } = &mut segments[index]
                {
                    *ctrl2 = opposite;
                }
            }
        }
    }
    *subpath = SubPath::new(segments, subpath.is_closed()).ok()?;
    Some(Path::new(subpaths))
}

fn nearest_node_control_handle(
    state: &AppState,
    node: NodeRef,
    at: Point,
) -> Option<(NodeHandleSide, Point, Option<Point>, bool)> {
    let shape = state.document.shape(node.shape_id)?;
    let path = editable_path(&shape.world_path());
    let (anchor, incoming, outgoing) = node_control_points(&path, node)?;
    let candidates = [
        (NodeHandleSide::Incoming, incoming, outgoing),
        (NodeHandleSide::Outgoing, outgoing, incoming),
    ];
    let tolerance = HANDLE_HIT_PX / state.viewport.zoom();
    candidates
        .into_iter()
        .filter_map(|(side, point, opposite)| {
            let point = point?;
            let distance = point.distance(at);
            (distance <= tolerance && distance > 1e-9).then_some((
                side,
                point,
                opposite,
                node_is_smooth(anchor, incoming, outgoing),
                distance,
            ))
        })
        .min_by(|a, b| a.4.total_cmp(&b.4))
        .map(|(side, point, opposite, smooth, _)| (side, point, opposite, smooth))
}

fn editable_anchors(subpath: &SubPath) -> Vec<(usize, Point)> {
    let segments = subpath.segments();
    let mut anchors = Vec::new();
    let count = if subpath.is_closed() {
        segments.len()
    } else {
        segments.len() + 1
    };
    for anchor in 0..count {
        let current = if anchor < segments.len() {
            Some(anchor)
        } else {
            None
        };
        let previous = if anchor > 0 {
            Some(anchor - 1)
        } else if subpath.is_closed() {
            Some(segments.len() - 1)
        } else {
            None
        };
        let editable = current.map_or(true, |i| !matches!(segments[i], Segment::Arc { .. }))
            && previous.map_or(true, |i| !matches!(segments[i], Segment::Arc { .. }));
        if editable {
            let point = current
                .map(|i| segments[i].start())
                .or_else(|| previous.map(|i| segments[i].end()));
            if let Some(point) = point {
                anchors.push((anchor, point));
            }
        }
    }
    anchors
}

/// Converts elliptical arcs to quarter-turn-or-smaller cubic Béziers so their
/// endpoints can be edited like other path nodes.
fn editable_path(path: &Path) -> Path {
    let subpaths = path
        .subpaths()
        .iter()
        .map(|subpath| {
            let segments = subpath
                .segments()
                .iter()
                .flat_map(|segment| match *segment {
                    Segment::Arc {
                        center,
                        radii,
                        x_rotation,
                        start_angle,
                        sweep_angle,
                    } => arc_as_cubics(center, radii, x_rotation, start_angle, sweep_angle),
                    other => vec![other],
                })
                .collect();
            SubPath::new(segments, subpath.is_closed()).unwrap_or_else(|_| subpath.clone())
        })
        .collect();
    Path::new(subpaths)
}

fn arc_as_cubics(
    center: Point,
    radii: Vec2,
    x_rotation: f64,
    start_angle: f64,
    sweep_angle: f64,
) -> Vec<Segment> {
    let quarter_turn = std::f64::consts::FRAC_PI_2;
    let piece_count = (sweep_angle.abs() / quarter_turn).ceil().max(1.0) as usize;
    let step = sweep_angle / piece_count as f64;
    let (cos_rotation, sin_rotation) = (x_rotation.cos(), x_rotation.sin());
    let point_at = |angle: f64| {
        let (cos_angle, sin_angle) = (angle.cos(), angle.sin());
        Point::new(
            center.x + cos_rotation * radii.x * cos_angle - sin_rotation * radii.y * sin_angle,
            center.y + sin_rotation * radii.x * cos_angle + cos_rotation * radii.y * sin_angle,
        )
    };
    let derivative_at = |angle: f64| {
        let (cos_angle, sin_angle) = (angle.cos(), angle.sin());
        Vec2::new(
            -cos_rotation * radii.x * sin_angle - sin_rotation * radii.y * cos_angle,
            -sin_rotation * radii.x * sin_angle + cos_rotation * radii.y * cos_angle,
        )
    };
    (0..piece_count)
        .map(|index| {
            let angle0 = start_angle + step * index as f64;
            let angle1 = angle0 + step;
            let from = point_at(angle0);
            let to = point_at(angle1);
            let scale = 4.0 / 3.0 * (step / 4.0).tan();
            let tangent0 = derivative_at(angle0);
            let tangent1 = derivative_at(angle1);
            Segment::CubicBezier {
                from,
                ctrl1: Point::new(from.x + tangent0.x * scale, from.y + tangent0.y * scale),
                ctrl2: Point::new(to.x - tangent1.x * scale, to.y - tangent1.y * scale),
                to,
            }
        })
        .collect()
}

fn finish_drag(state: &mut AppState, end: Point, shift: bool) {
    let interaction = std::mem::take(&mut state.interaction);
    match interaction {
        Interaction::DrawShape { tool, start } => {
            if tool == CanvasTool::Line {
                let _ = (start, end, shift);
            } else {
                let bounds = shape_bounds(start, end, shift);
                if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
                    return;
                }
                let (kind, transform) = match tool {
                    CanvasTool::Rectangle => (
                        RectShape::new(bounds.width(), bounds.height(), 0.0).map(ShapeKind::Rect),
                        Transform::translate(bounds.min.x, bounds.min.y),
                    ),
                    CanvasTool::Ellipse => (
                        EllipseShape::new(bounds.width() / 2.0, bounds.height() / 2.0)
                            .map(ShapeKind::Ellipse),
                        Transform::translate(bounds.center().x, bounds.center().y),
                    ),
                    CanvasTool::Polygon => (
                        regular_polygon(bounds, 6, false).map(ShapeKind::Polygon),
                        Transform::identity(),
                    ),
                    CanvasTool::Star => (
                        regular_polygon(bounds, 5, true).map(ShapeKind::Polygon),
                        Transform::identity(),
                    ),
                    CanvasTool::Select
                    | CanvasTool::EditNodes
                    | CanvasTool::Line
                    | CanvasTool::RoundCorners => return,
                };
                match kind {
                    Ok(kind) => actions::create_shape(state, kind, transform),
                    Err(err) => state.set_status(
                        crate::state::StatusLevel::Error,
                        format!("Could not create shape: {err}"),
                    ),
                }
            }
        }
        Interaction::MoveNode { node, start } => {
            let delta = move_delta(start, end);
            if !delta.is_identity() {
                let nodes = if state.selected_nodes.contains(&node) {
                    state.selected_nodes.clone()
                } else {
                    vec![node]
                };
                let shape_ids: HashSet<ShapeId> = nodes.iter().map(|node| node.shape_id).collect();
                for shape_id in shape_ids {
                    if let Some(shape) = state.document.shape(shape_id) {
                        let mut path = editable_path(&shape.world_path());
                        for selected in nodes
                            .iter()
                            .filter(|selected| selected.shape_id == shape_id)
                        {
                            if let Some(updated) = move_path_anchor(
                                &path,
                                selected.subpath,
                                selected.anchor,
                                Vec2::new(delta.e, delta.f),
                            ) {
                                path = updated;
                            }
                        }
                        actions::set_shape_geometry(
                            state,
                            shape_id,
                            ShapeKind::Path(path),
                            Transform::identity(),
                        );
                    }
                }
            }
            state.selected_node = Some(node);
        }
        Interaction::MoveNodeHandle {
            handle,
            pointer_offset,
            opposite_length,
            smooth,
        } => {
            let target = Point::new(end.x + pointer_offset.x, end.y + pointer_offset.y);
            if let Some(shape) = state.document.shape(handle.node.shape_id) {
                let path = editable_path(&shape.world_path());
                if let Some(path) = move_path_control(
                    &path,
                    handle.node,
                    handle.side,
                    target,
                    opposite_length,
                    smooth,
                ) {
                    actions::set_shape_geometry(
                        state,
                        handle.node.shape_id,
                        ShapeKind::Path(path),
                        Transform::identity(),
                    );
                }
            }
            state.selected_node = Some(handle.node);
        }
        Interaction::Marquee { start } => {
            let mode = marquee_mode(start, end);
            let found = shapes_in_rect(&state.document, Rect::new(start, end), mode);
            if shift {
                add_unique(&mut state.selection, found);
            } else {
                state.selection = found;
            }
        }
        _ => {
            if let Some(delta) = drag_delta(&interaction, end, shift) {
                actions::transform_selection(state, delta, false);
            }
        }
    }
}

fn finish_line(state: &mut AppState, closed: bool) {
    let points = std::mem::take(&mut state.line_points);
    let min_points = if closed { 3 } else { 2 };
    if points.len() < min_points {
        return;
    }
    match PolygonShape::new(points, closed) {
        Ok(line) => actions::create_shape(state, ShapeKind::Polygon(line), Transform::identity()),
        Err(err) => state.set_status(
            crate::state::StatusLevel::Error,
            format!("Could not create line: {err}"),
        ),
    }
}

fn should_close_line(state: &AppState, pointer: Point) -> bool {
    if state.line_points.len() < 3 {
        return false;
    }
    let start = state.line_points[0];
    state
        .viewport
        .to_screen(start)
        .distance(state.viewport.to_screen(pointer))
        <= LINE_CLOSE_SNAP_PX
}

fn click_select(state: &mut AppState, at: Point, shift: bool) {
    state.selected_node = None;
    state.selected_nodes.clear();
    state.selected_segment = None;
    state.selected_segment_point = None;
    let zoom = state.viewport.zoom();
    match pick_shape(&state.document, at, PICK_TOLERANCE_PX / zoom) {
        Some(id) if shift => {
            if let Some(i) = state.selection.iter().position(|s| *s == id) {
                state.selection.remove(i);
            } else {
                state.selection.push(id);
            }
        }
        Some(id) => state.selection = vec![id],
        None if !shift => state.selection.clear(),
        None => {}
    }
}

/// The transform a drag would apply if released at `pointer` (mm).
fn drag_delta(interaction: &Interaction, pointer: Point, shift: bool) -> Option<Transform> {
    let delta = match *interaction {
        Interaction::Idle
        | Interaction::Marquee { .. }
        | Interaction::DrawShape { .. }
        | Interaction::MoveNode { .. }
        | Interaction::MoveNodeHandle { .. } => {
            return None;
        }
        Interaction::Move { start } => move_delta(start, pointer),
        Interaction::Scale {
            handle,
            start,
            bounds,
        } => scale_delta(bounds, handle, start, pointer, shift),
        Interaction::Rotate { start, center } => {
            let snap = shift.then(|| ROTATE_SNAP_DEG.to_radians());
            rotate_delta(center, start, pointer, snap)
        }
    };
    Some(delta)
}

/// Dragging right selects shapes fully inside; dragging left selects
/// shapes touched (the usual CAD convention).
fn marquee_mode(start: Point, end: Point) -> MarqueeMode {
    if end.x >= start.x {
        MarqueeMode::Window
    } else {
        MarqueeMode::Crossing
    }
}

fn add_unique(selection: &mut Vec<ShapeId>, ids: Vec<ShapeId>) {
    let present: HashSet<ShapeId> = selection.iter().copied().collect();
    selection.extend(ids.into_iter().filter(|id| !present.contains(id)));
}

fn cursor_for(state: &AppState, hover_px: Option<Point>) -> Option<CursorIcon> {
    match state.interaction {
        Interaction::DrawShape { .. } => return Some(CursorIcon::Crosshair),
        Interaction::MoveNode { .. } => return Some(CursorIcon::Grabbing),
        Interaction::MoveNodeHandle { .. } => return Some(CursorIcon::Grabbing),
        Interaction::Move { .. } => return Some(CursorIcon::Grabbing),
        Interaction::Rotate { .. } => return Some(CursorIcon::Grabbing),
        Interaction::Scale { handle, .. } => return Some(resize_cursor(handle)),
        Interaction::Marquee { .. } => return None,
        Interaction::Idle => {}
    }
    let vp = &state.viewport;
    let at = vp.to_world(hover_px?);
    if let Some(node) = state.selected_node
        && nearest_node_control_handle(state, node, at).is_some()
    {
        return Some(CursorIcon::Grabbing);
    }
    let bounds = display_bounds(state)?;
    let zoom = vp.zoom();
    match hit_handle(bounds, at, HANDLE_HIT_PX / zoom, ROTATE_OFFSET_PX / zoom) {
        Some(handle) => Some(resize_cursor(handle)),
        None if bounds.contains_point(at) => Some(CursorIcon::Move),
        None => None,
    }
}

fn shape_bounds(start: Point, end: Point, constrain: bool) -> Rect {
    let end = if constrain {
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let side = dx.abs().max(dy.abs());
        Point::new(start.x + side.copysign(dx), start.y + side.copysign(dy))
    } else {
        end
    };
    Rect::new(start, end)
}

fn constrain_line(start: Point, end: Point, constrain: bool) -> Point {
    if !constrain {
        return end;
    }
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length = dx.hypot(dy);
    if length == 0.0 {
        return end;
    }
    let step = std::f64::consts::FRAC_PI_4;
    let angle = (dy.atan2(dx) / step).round() * step;
    Point::new(
        start.x + length * angle.cos(),
        start.y + length * angle.sin(),
    )
}

fn draw_shape_preview(
    painter: &Painter,
    state: &AppState,
    pointer: Option<Point>,
    constrain: bool,
) {
    if state.canvas_tool == CanvasTool::Line {
        let mut points: Vec<Pos2> = state
            .line_points
            .iter()
            .map(|&point| to_pos(state.viewport.to_screen(point)))
            .collect();
        if let (Some(&last), Some(end)) = (state.line_points.last(), pointer) {
            let close = should_close_line(state, end);
            let end = if close {
                state.line_points[0]
            } else {
                constrain_line(last, end, constrain)
            };
            points.push(to_pos(state.viewport.to_screen(end)));
            if close {
                painter.circle_stroke(
                    to_pos(state.viewport.to_screen(state.line_points[0])),
                    LINE_CLOSE_SNAP_PX as f32,
                    Stroke::new(1.5, CROSSING_COLOR),
                );
            }
        }
        if points.len() >= 2 {
            painter.add(egui::Shape::line(points, Stroke::new(1.5, SELECTION_COLOR)));
        }
        return;
    }
    let (Interaction::DrawShape { tool, start }, Some(end)) = (state.interaction, pointer) else {
        return;
    };
    let stroke = Stroke::new(1.5, SELECTION_COLOR);
    let bounds = shape_bounds(start, end, constrain);
    if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return;
    }
    let viewport = &state.viewport;
    let frame = egui::Rect::from_two_pos(
        to_pos(viewport.to_screen(Point::new(bounds.min.x, bounds.max.y))),
        to_pos(viewport.to_screen(Point::new(bounds.max.x, bounds.min.y))),
    );
    match tool {
        CanvasTool::Rectangle => {
            painter.rect_stroke(frame, 0.0, stroke, egui::StrokeKind::Middle);
        }
        CanvasTool::Ellipse => {
            let points = radial_points(frame.center(), frame.size() / 2.0, 64, None);
            painter.add(egui::Shape::closed_line(points, stroke));
        }
        CanvasTool::Polygon => {
            let points = radial_points(frame.center(), frame.size() / 2.0, 6, None);
            painter.add(egui::Shape::closed_line(points, stroke));
        }
        CanvasTool::Star => {
            let points = radial_points(frame.center(), frame.size() / 2.0, 10, Some(0.5));
            painter.add(egui::Shape::closed_line(points, stroke));
        }
        CanvasTool::Select
        | CanvasTool::Line
        | CanvasTool::EditNodes
        | CanvasTool::RoundCorners => {}
    }
}

fn draw_node_preview(painter: &Painter, state: &AppState, pointer: Option<Point>) {
    let Some(pointer) = pointer else {
        return;
    };
    let path = match state.interaction {
        Interaction::MoveNode { node, start } => {
            let Some(shape) = state.document.shape(node.shape_id) else {
                return;
            };
            let delta = Vec2::new(pointer.x - start.x, pointer.y - start.y);
            let nodes = if state.selected_nodes.contains(&node) {
                state.selected_nodes.clone()
            } else {
                vec![node]
            };
            let mut path = editable_path(&shape.world_path());
            for selected in nodes {
                if selected.shape_id == node.shape_id
                    && let Some(updated) =
                        move_path_anchor(&path, selected.subpath, selected.anchor, delta)
                {
                    path = updated;
                }
            }
            path
        }
        Interaction::MoveNodeHandle {
            handle,
            pointer_offset,
            opposite_length,
            smooth,
        } => {
            let Some(shape) = state.document.shape(handle.node.shape_id) else {
                return;
            };
            let path = editable_path(&shape.world_path());
            let target = Point::new(pointer.x + pointer_offset.x, pointer.y + pointer_offset.y);
            let Some(path) = move_path_control(
                &path,
                handle.node,
                handle.side,
                target,
                opposite_length,
                smooth,
            ) else {
                return;
            };
            path
        }
        _ => return,
    };
    let Ok(polylines) = pf_core::flatten_path(&path, 0.25) else {
        return;
    };
    let vp = &state.viewport;
    for polyline in polylines {
        let points: Vec<Pos2> = polyline
            .points
            .iter()
            .map(|&point| to_pos(vp.to_screen(point)))
            .collect();
        if polyline.closed {
            painter.add(egui::Shape::closed_line(
                points,
                Stroke::new(1.5, SELECTION_COLOR),
            ));
        } else {
            painter.add(egui::Shape::line(points, Stroke::new(1.5, SELECTION_COLOR)));
        }
    }
}

fn draw_node_handles(painter: &Painter, state: &AppState, pointer: Option<Point>) {
    let editing_nodes = state.canvas_tool == CanvasTool::EditNodes;
    let rounding_corners = state.canvas_tool == CanvasTool::RoundCorners;
    if !editing_nodes && !rounding_corners {
        return;
    }
    let selected: HashSet<ShapeId> = state.selection.iter().copied().collect();
    if editing_nodes
        && let Some(reference) = state.selected_segment
        && let Some(shape) = state.document.shape(reference.shape_id)
        && let Some(segment) = editable_path(&shape.world_path())
            .subpaths()
            .get(reference.subpath)
            .and_then(|subpath| subpath.segments().get(reference.segment))
    {
        let mut last = segment.point_at(0.0);
        for step in 1..=24 {
            let next = segment.point_at(f64::from(step) / 24.0);
            painter.line_segment(
                [
                    to_pos(state.viewport.to_screen(last)),
                    to_pos(state.viewport.to_screen(next)),
                ],
                Stroke::new(3.0, SELECTION_COLOR),
            );
            last = next;
        }
    }
    for shape in state
        .document
        .shapes()
        .iter()
        .filter(|shape| selected.contains(&shape.id))
    {
        let path = editable_path(&shape.world_path());
        for (subpath_index, subpath) in path.subpaths().iter().enumerate() {
            let anchors = if rounding_corners {
                roundable_anchors(subpath)
                    .into_iter()
                    .filter_map(|anchor| anchor_point(subpath, anchor).map(|point| (anchor, point)))
                    .collect()
            } else {
                editable_anchors(subpath)
            };
            for (anchor, point) in anchors {
                let node = NodeRef {
                    shape_id: shape.id,
                    subpath: subpath_index,
                    anchor,
                };
                let center = to_pos(state.viewport.to_screen(point));
                let is_selected = if rounding_corners {
                    state.rounding_nodes.contains(&node)
                } else {
                    state.selected_node == Some(node) || state.selected_nodes.contains(&node)
                };
                let fill = if is_selected {
                    SELECTION_COLOR
                } else {
                    Color32::WHITE
                };
                let radius = if rounding_corners { 5.0 } else { 4.0 };
                painter.circle(center, radius, fill, Stroke::new(1.0, SELECTION_COLOR));
            }
        }
    }
    if let Some(node) = state.selected_node
        && let Some(shape) = state.document.shape(node.shape_id)
    {
        let mut path = editable_path(&shape.world_path());
        if let (
            Interaction::MoveNodeHandle {
                handle,
                pointer_offset,
                opposite_length,
                smooth,
            },
            Some(pointer),
        ) = (state.interaction, pointer)
            && handle.node == node
            && let Some(updated) = move_path_control(
                &path,
                node,
                handle.side,
                Point::new(pointer.x + pointer_offset.x, pointer.y + pointer_offset.y),
                opposite_length,
                smooth,
            )
        {
            path = updated;
        }
        if let Some((anchor, incoming, outgoing)) = node_control_points(&path, node) {
            let anchor_screen = to_pos(state.viewport.to_screen(anchor));
            for (side, control) in [
                (NodeHandleSide::Incoming, incoming),
                (NodeHandleSide::Outgoing, outgoing),
            ] {
                let Some(control) = control.filter(|point| point.distance(anchor) > 1e-6) else {
                    continue;
                };
                let screen = to_pos(state.viewport.to_screen(control));
                painter.line_segment(
                    [anchor_screen, screen],
                    Stroke::new(1.0, SELECTION_COLOR.gamma_multiply(0.8)),
                );
                let rect = egui::Rect::from_center_size(screen, egui::vec2(7.0, 7.0));
                let fill = match state.interaction {
                    Interaction::MoveNodeHandle { handle, .. }
                        if handle.node == node && handle.side == side =>
                    {
                        SELECTION_COLOR
                    }
                    _ => Color32::WHITE,
                };
                painter.rect_filled(rect, 0.0, fill);
                painter.rect_stroke(
                    rect,
                    0.0,
                    Stroke::new(1.0, SELECTION_COLOR),
                    egui::StrokeKind::Middle,
                );
            }
        }
    }
}

fn move_path_anchor(path: &Path, subpath_index: usize, anchor: usize, delta: Vec2) -> Option<Path> {
    let mut subpaths = path.subpaths().to_vec();
    let subpath = subpaths.get_mut(subpath_index)?;
    let count = subpath.segments().len();
    let anchor_count = count + if subpath.is_closed() { 0 } else { 1 };
    if count == 0 || anchor >= anchor_count {
        return None;
    }
    let segments = subpath.segments().to_vec();
    let current = (anchor < count).then_some(anchor);
    let previous = if anchor > 0 {
        Some(anchor - 1)
    } else if subpath.is_closed() {
        Some(count - 1)
    } else {
        None
    };
    if current.is_some_and(|i| matches!(segments[i], Segment::Arc { .. }))
        || previous.is_some_and(|i| matches!(segments[i], Segment::Arc { .. }))
    {
        return None;
    }
    let offset = |point: Point| Point::new(point.x + delta.x, point.y + delta.y);
    let mut updated = segments;
    if current.is_some() && current == previous {
        set_segment_both(&mut updated[current?], offset);
    } else {
        if let Some(index) = current {
            set_segment_start(&mut updated[index], offset);
        }
        if let Some(index) = previous {
            set_segment_end(&mut updated[index], offset);
        }
    }
    let closed = subpath.is_closed();
    *subpath = SubPath::new(updated, closed).ok()?;
    Some(Path::new(subpaths))
}

fn set_segment_both(segment: &mut Segment, offset: impl Fn(Point) -> Point) {
    match segment {
        Segment::Line { from, to } => {
            *from = offset(*from);
            *to = offset(*to);
        }
        Segment::QuadBezier { from, ctrl, to } => {
            *from = offset(*from);
            *ctrl = offset(*ctrl);
            *to = offset(*to);
        }
        Segment::CubicBezier {
            from,
            ctrl1,
            ctrl2,
            to,
        } => {
            *from = offset(*from);
            *ctrl1 = offset(*ctrl1);
            *ctrl2 = offset(*ctrl2);
            *to = offset(*to);
        }
        Segment::Arc { .. } => {}
    }
}

fn set_segment_start(segment: &mut Segment, offset: impl Fn(Point) -> Point) {
    match segment {
        Segment::Line { from, .. } => *from = offset(*from),
        Segment::QuadBezier { from, ctrl, .. } => {
            *from = offset(*from);
            *ctrl = offset(*ctrl);
        }
        Segment::CubicBezier { from, ctrl1, .. } => {
            *from = offset(*from);
            *ctrl1 = offset(*ctrl1);
        }
        Segment::Arc { .. } => {}
    }
}

fn set_segment_end(segment: &mut Segment, offset: impl Fn(Point) -> Point) {
    match segment {
        Segment::Line { to, .. } => *to = offset(*to),
        Segment::QuadBezier { ctrl, to, .. } => {
            *ctrl = offset(*ctrl);
            *to = offset(*to);
        }
        Segment::CubicBezier { ctrl2, to, .. } => {
            *ctrl2 = offset(*ctrl2);
            *to = offset(*to);
        }
        Segment::Arc { .. } => {}
    }
}

fn regular_polygon(
    bounds: Rect,
    points: u32,
    star: bool,
) -> Result<PolygonShape, pf_core::ShapeError> {
    let vertex_count = if star {
        points as usize * 2
    } else {
        points as usize
    };
    let points = (0..vertex_count)
        .map(|index| {
            let angle = -std::f64::consts::FRAC_PI_2
                + std::f64::consts::TAU * index as f64 / vertex_count as f64;
            let radius = if star && index % 2 == 1 { 0.5 } else { 1.0 };
            Point::new(
                bounds.center().x + bounds.width() / 2.0 * radius * angle.cos(),
                bounds.center().y + bounds.height() / 2.0 * radius * angle.sin(),
            )
        })
        .collect();
    PolygonShape::new(points, true)
}

fn radial_points(
    center: Pos2,
    radius: egui::Vec2,
    vertices: usize,
    inner_radius: Option<f32>,
) -> Vec<Pos2> {
    (0..vertices)
        .map(|index| {
            let angle = -std::f32::consts::FRAC_PI_2
                + std::f32::consts::TAU * index as f32 / vertices as f32;
            let scale = if index % 2 == 1 {
                inner_radius.unwrap_or(1.0)
            } else {
                1.0
            };
            center
                + egui::vec2(
                    radius.x * scale * angle.cos(),
                    radius.y * scale * angle.sin(),
                )
        })
        .collect()
}

fn resize_cursor(handle: Handle) -> CursorIcon {
    match handle {
        Handle::N | Handle::S => CursorIcon::ResizeVertical,
        Handle::E | Handle::W => CursorIcon::ResizeHorizontal,
        Handle::NE | Handle::SW => CursorIcon::ResizeNeSw,
        Handle::NW | Handle::SE => CursorIcon::ResizeNwSe,
        Handle::Rotate => CursorIcon::Grab,
    }
}

/// Selection bounds from the display cache, cheap enough for every frame.
fn display_bounds(state: &AppState) -> Option<Rect> {
    if state.selection.is_empty() {
        return None;
    }
    let selected: HashSet<ShapeId> = state.selection.iter().copied().collect();
    state
        .display
        .shapes()
        .iter()
        .filter(|s| selected.contains(&s.id))
        .map(|s| s.bounds)
        .reduce(|a, b| a.union(&b))
}

fn draw_bed(painter: &Painter, state: &AppState, screen: Rect) {
    let vp = &state.viewport;
    let bed = state.profile.work_area();
    let bed_px =
        egui::Rect::from_two_pos(to_pos(vp.to_screen(bed.min)), to_pos(vp.to_screen(bed.max)));
    painter.rect_filled(bed_px, 0.0, BED_COLOR);

    // Grid only inside the bed, and only where it is visible.
    let visible = vp.visible_world_rect(screen);
    if visible.intersects(&bed) {
        let area = Rect::new(
            Point::new(bed.min.x.max(visible.min.x), bed.min.y.max(visible.min.y)),
            Point::new(bed.max.x.min(visible.max.x), bed.max.y.min(visible.max.y)),
        );
        let step = grid_step(vp.zoom(), MIN_GRID_PX);
        for (x, major) in grid_lines(area.min.x, area.max.x, step) {
            let a = vp.to_screen(Point::new(x, bed.min.y));
            let b = vp.to_screen(Point::new(x, bed.max.y));
            painter.line_segment([to_pos(a), to_pos(b)], grid_stroke(major));
        }
        for (y, major) in grid_lines(area.min.y, area.max.y, step) {
            let a = vp.to_screen(Point::new(bed.min.x, y));
            let b = vp.to_screen(Point::new(bed.max.x, y));
            painter.line_segment([to_pos(a), to_pos(b)], grid_stroke(major));
        }
    }

    // With the preview on, a job that leaves the bed shows a red border.
    let out_of_bounds = state.show_preview
        && matches!(
            state.preview.result(),
            Some(Err(GcodeError::OutOfBounds { .. }))
        );
    let border = if out_of_bounds {
        Stroke::new(2.0, OUT_OF_BOUNDS_COLOR)
    } else {
        Stroke::new(1.0, BED_BORDER)
    };
    painter.rect_stroke(bed_px, 0.0, border, egui::StrokeKind::Outside);

    let origin = to_pos(vp.to_screen(Point::ORIGIN));
    painter.line_segment(
        [origin, origin + egui::vec2(ORIGIN_MARK_PX, 0.0)],
        Stroke::new(2.0, Color32::RED),
    );
    painter.line_segment(
        [origin, origin - egui::vec2(0.0, ORIGIN_MARK_PX)],
        Stroke::new(2.0, Color32::from_rgb(0, 160, 0)),
    );
}

/// Grid positions (mm) in `[from, to]` with whether each is a major line.
fn grid_lines(from: f64, to: f64, step: f64) -> impl Iterator<Item = (f64, bool)> {
    let first = (from / step).ceil() as i64;
    let last = (to / step).floor() as i64;
    (first..=last).map(move |i| (i as f64 * step, i % MAJOR_EVERY == 0))
}

fn grid_stroke(major: bool) -> Stroke {
    Stroke::new(1.0, if major { GRID_MAJOR } else { GRID_MINOR })
}

/// Selected shapes are dashed and drawn through `preview`, the transform
/// of a drag in progress.
fn draw_shapes(painter: &Painter, state: &AppState, screen: Rect, preview: Option<Transform>) {
    let vp = &state.viewport;
    let visible = vp.visible_world_rect(screen);
    let selected: HashSet<ShapeId> = state.selection.iter().copied().collect();
    for shape in state.display.shapes() {
        let is_selected = selected.contains(&shape.id);
        let moving = preview.filter(|_| is_selected);
        let bounds = moving.map_or(shape.bounds, |t| t.transform_rect(&shape.bounds));
        if !bounds.intersects(&visible) {
            continue;
        }
        let [r, g, b] = shape.color;
        let color = Color32::from_rgb(r, g, b);
        for polyline in &shape.polylines {
            let points: Vec<Pos2> = polyline
                .points
                .iter()
                .map(|&p| {
                    let world = moving.map_or(p, |t| t.apply_point(p));
                    to_pos(vp.to_screen(world))
                })
                .collect();
            if is_selected {
                painter.extend(egui::Shape::dashed_line(
                    &points,
                    Stroke::new(1.5, color),
                    4.0,
                    3.0,
                ));
            } else {
                painter.add(egui::Shape::line(points, Stroke::new(1.0, color)));
            }
        }
    }
}

/// A crosshair where the connected machine's laser head is. Machine
/// coordinates equal document coordinates (bottom-left origin).
fn draw_laser_head(painter: &Painter, state: &AppState) {
    let Some([x, y, _]) = state.device.snapshot.machine_position_mm() else {
        return;
    };
    let center = to_pos(state.viewport.to_screen(Point::new(x, y)));
    let stroke = Stroke::new(1.5, LASER_HEAD_COLOR);
    let r = LASER_HEAD_PX;
    painter.circle_stroke(center, r * 0.6, stroke);
    painter.line_segment(
        [center - egui::vec2(r, 0.0), center + egui::vec2(r, 0.0)],
        stroke,
    );
    painter.line_segment(
        [center - egui::vec2(0.0, r), center + egui::vec2(0.0, r)],
        stroke,
    );
}

/// The planned job over the canvas: cuts in their layer color, travel moves
/// dashed. Toolpaths are in machine coordinates, so they are shifted back by
/// the job origin.
fn draw_toolpaths(painter: &Painter, state: &AppState) {
    let Some(Ok(preview)) = state.preview.result() else {
        return;
    };
    let vp = &state.viewport;
    let zero = state.job_options.origin.document_zero();
    let screen = |p: Point| to_pos(vp.to_screen(Point::new(p.x + zero.x, p.y + zero.y)));
    let colors: HashMap<LayerId, [u8; 3]> = state
        .document
        .layers()
        .iter()
        .map(|l| (l.id, l.color))
        .collect();

    for (layer, points) in &preview.cuts {
        let [r, g, b] = colors.get(layer).copied().unwrap_or([0, 0, 0]);
        let points = points.iter().map(|&p| screen(p)).collect();
        painter.add(egui::Shape::line(
            points,
            Stroke::new(2.0, Color32::from_rgb(r, g, b)),
        ));
    }
    if state.show_travels {
        for &(from, to) in &preview.travels {
            painter.extend(egui::Shape::dashed_line(
                &[screen(from), screen(to)],
                Stroke::new(1.0, TRAVEL_COLOR),
                4.0,
                4.0,
            ));
        }
    }
}

fn draw_selection_frame(painter: &Painter, state: &AppState, preview: Option<Transform>) {
    let Some(bounds) = display_bounds(state) else {
        return;
    };
    let bounds = preview.map_or(bounds, |t| t.transform_rect(&bounds));
    let vp = &state.viewport;
    let frame = egui::Rect::from_two_pos(
        to_pos(vp.to_screen(bounds.min)),
        to_pos(vp.to_screen(bounds.max)),
    );
    let stroke = Stroke::new(1.0, SELECTION_COLOR);
    painter.rect_stroke(frame, 0.0, stroke, egui::StrokeKind::Middle);
    if state.interaction != Interaction::Idle {
        return;
    }

    let rotate_offset = ROTATE_OFFSET_PX / vp.zoom();
    let top = to_pos(vp.to_screen(handle_position(bounds, Handle::N, rotate_offset)));
    let knob = to_pos(vp.to_screen(handle_position(bounds, Handle::Rotate, rotate_offset)));
    painter.line_segment([top, knob], stroke);
    painter.circle(knob, HANDLE_SIZE_PX / 2.0, Color32::WHITE, stroke);
    for handle in Handle::ALL {
        if handle == Handle::Rotate {
            continue;
        }
        let center = to_pos(vp.to_screen(handle_position(bounds, handle, rotate_offset)));
        let square = egui::Rect::from_center_size(center, egui::Vec2::splat(HANDLE_SIZE_PX));
        painter.rect_filled(square, 0.0, Color32::WHITE);
        painter.rect_stroke(square, 0.0, stroke, egui::StrokeKind::Inside);
    }
}

fn draw_marquee(painter: &Painter, vp: &Viewport, start: Point, end: Point) {
    let color = match marquee_mode(start, end) {
        MarqueeMode::Window => SELECTION_COLOR,
        MarqueeMode::Crossing => CROSSING_COLOR,
    };
    let rect = egui::Rect::from_two_pos(to_pos(vp.to_screen(start)), to_pos(vp.to_screen(end)));
    painter.rect_filled(rect, 0.0, color.gamma_multiply(0.1));
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, color), egui::StrokeKind::Middle);
}

fn draw_frame_time(ui: &egui::Ui, painter: &Painter, rect: egui::Rect) {
    let ms = ui.input(|i| i.stable_dt) * 1000.0;
    painter.text(
        rect.left_top() + egui::vec2(8.0, 8.0),
        egui::Align2::LEFT_TOP,
        format!("{ms:.1} ms/frame"),
        egui::FontId::monospace(12.0),
        Color32::YELLOW,
    );
}

fn to_pos(p: Point) -> Pos2 {
    Pos2::new(p.x as f32, p.y as f32)
}

fn to_point(p: Pos2) -> Point {
    Point::new(f64::from(p.x), f64::from(p.y))
}

fn to_rect(r: egui::Rect) -> Rect {
    Rect::new(to_point(r.min), to_point(r.max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pf_core::{Command, Document, RectShape, ShapeKind};

    /// Pixel tolerances become tenths of a millimeter at this zoom.
    const ZOOM: f64 = 10.0;

    /// Two 10×10 squares at (10,10) and (40,10).
    fn state_with_squares() -> (AppState, ShapeId, ShapeId) {
        let mut doc = Document::new();
        let layer = doc.add_layer("Cut", [0, 0, 0]).unwrap();
        let square = || ShapeKind::Rect(RectShape::new(10.0, 10.0, 0.0).unwrap());
        doc.add_shape(square(), Transform::translate(10.0, 10.0), layer)
            .unwrap();
        doc.add_shape(square(), Transform::translate(40.0, 10.0), layer)
            .unwrap();
        let mut state = AppState::new();
        state
            .history
            .apply(&mut state.document, Command::ImportDocument(doc))
            .unwrap();
        state.selection.clear();
        state.viewport = Viewport::new(Point::ORIGIN, ZOOM);
        state
            .display
            .sync(&state.document, state.history.revision());
        let ids = state.document.shapes();
        let (a, b) = (ids[0].id, ids[1].id);
        (state, a, b)
    }

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn rectangle_tool_creates_a_shape_and_undo_removes_it() {
        let mut state = AppState::new();
        state.canvas_tool = CanvasTool::Rectangle;
        state.interaction = Interaction::DrawShape {
            tool: CanvasTool::Rectangle,
            start: p(20.0, 30.0),
        };
        finish_drag(&mut state, p(10.0, 12.0), false);

        assert_eq!(state.document.shapes().len(), 1);
        assert_eq!(state.document.layers().len(), 1);
        assert_eq!(state.selection, vec![state.document.shapes()[0].id]);
        assert!(
            state.document.shapes()[0]
                .bounds()
                .unwrap()
                .approx_eq(&Rect::new(p(10.0, 12.0), p(20.0, 30.0)), 1e-9)
        );
        assert_eq!(state.history.undo_label(), Some("Draw shape"));

        actions::undo(&mut state);
        assert!(state.document.shapes().is_empty());
        assert!(state.document.layers().is_empty());
    }

    #[test]
    fn ellipse_tool_uses_selected_layer_and_shift_constrains_to_circle() {
        let (mut state, _, _) = state_with_squares();
        let layer = state.document.layers()[0].id;
        state.selected_layer = Some(layer);
        state.canvas_tool = CanvasTool::Ellipse;
        state.interaction = Interaction::DrawShape {
            tool: CanvasTool::Ellipse,
            start: p(5.0, 10.0),
        };
        finish_drag(&mut state, p(15.0, 20.0), true);

        let shape = &state.document.shapes()[2];
        assert_eq!(shape.layer_id, layer);
        assert!(matches!(
            shape.kind,
            ShapeKind::Ellipse(ellipse)
                if (ellipse.rx() - 5.0).abs() < 1e-9 && (ellipse.ry() - 5.0).abs() < 1e-9
        ));
        assert!(
            shape
                .bounds()
                .unwrap()
                .approx_eq(&Rect::new(p(5.0, 10.0), p(15.0, 20.0)), 1e-9)
        );
        assert_eq!(state.selection, vec![shape.id]);
    }

    fn translation(state: &AppState, id: ShapeId) -> (f64, f64) {
        let t = state.document.shape(id).unwrap().transform;
        (t.e, t.f)
    }

    #[test]
    fn click_selects_toggles_and_clears() {
        let (mut state, a, b) = state_with_squares();
        click_select(&mut state, p(10.0, 15.0), false);
        assert_eq!(state.selection, vec![a]);
        click_select(&mut state, p(50.0, 15.0), true);
        assert_eq!(state.selection, vec![a, b]);
        click_select(&mut state, p(10.0, 15.0), true);
        assert_eq!(state.selection, vec![b]);
        click_select(&mut state, p(100.0, 100.0), true);
        assert_eq!(state.selection, vec![b]);
        click_select(&mut state, p(100.0, 100.0), false);
        assert!(state.selection.is_empty());
    }

    #[test]
    fn dragging_an_unselected_shape_selects_and_moves_it() {
        let (mut state, a, b) = state_with_squares();
        state.selection = vec![b];
        state.interaction = begin_drag(&mut state, p(10.0, 12.0), false);
        assert_eq!(
            state.interaction,
            Interaction::Move {
                start: p(10.0, 12.0)
            }
        );
        assert_eq!(state.selection, vec![a]);
        finish_drag(&mut state, p(15.0, 20.0), false);
        assert_eq!(state.interaction, Interaction::Idle);
        assert_eq!(translation(&state, a), (15.0, 18.0));
        assert_eq!(translation(&state, b), (40.0, 10.0));
        assert_eq!(state.history.undo_label(), Some("Transform"));
    }

    #[test]
    fn drag_inside_the_selection_moves_everything_selected() {
        let (mut state, a, b) = state_with_squares();
        state.selection = vec![a, b];
        // Empty space between the squares, but inside the selection bounds.
        let interaction = begin_drag(&mut state, p(30.0, 15.0), false);
        assert_eq!(
            interaction,
            Interaction::Move {
                start: p(30.0, 15.0)
            }
        );
        state.interaction = interaction;
        finish_drag(&mut state, p(30.0, 5.0), false);
        assert_eq!(translation(&state, a), (10.0, 0.0));
        assert_eq!(translation(&state, b), (40.0, 0.0));
    }

    #[test]
    fn handles_start_scale_and_rotate() {
        let (mut state, a, _) = state_with_squares();
        state.selection = vec![a];
        let ne = begin_drag(&mut state, p(20.0, 20.0), false);
        assert!(matches!(
            ne,
            Interaction::Scale {
                handle: Handle::NE,
                ..
            }
        ));
        let knob = p(15.0, 20.0 + ROTATE_OFFSET_PX / ZOOM);
        let rotate = begin_drag(&mut state, knob, false);
        assert_eq!(
            rotate,
            Interaction::Rotate {
                start: knob,
                center: p(15.0, 15.0)
            }
        );

        // Doubling via the NE handle keeps the SW corner at (10, 10).
        state.interaction = ne;
        finish_drag(&mut state, p(30.0, 30.0), false);
        let bounds = state.document.shape(a).unwrap().bounds().unwrap();
        assert!(bounds.approx_eq(&Rect::new(p(10.0, 10.0), p(30.0, 30.0)), 1e-9));
    }

    #[test]
    fn marquee_direction_picks_window_or_crossing() {
        let (mut state, a, b) = state_with_squares();
        // Left to right around `a` only, ending inside `b`.
        state.interaction = begin_drag(&mut state, p(5.0, 5.0), false);
        assert_eq!(
            state.interaction,
            Interaction::Marquee { start: p(5.0, 5.0) }
        );
        finish_drag(&mut state, p(45.0, 25.0), false);
        assert_eq!(state.selection, vec![a]);
        // Right to left over the same area also catches `b`.
        state.interaction = Interaction::Marquee {
            start: p(45.0, 25.0),
        };
        finish_drag(&mut state, p(5.0, 5.0), false);
        assert_eq!(state.selection, vec![a, b]);
        // Shift adds without duplicates.
        state.selection = vec![b];
        state.interaction = Interaction::Marquee { start: p(5.0, 5.0) };
        finish_drag(&mut state, p(45.0, 25.0), true);
        assert_eq!(state.selection, vec![b, a]);
    }

    #[test]
    fn shift_snaps_rotation_and_keeps_aspect() {
        let center = p(0.0, 0.0);
        let rotate = Interaction::Rotate {
            start: p(10.0, 0.0),
            center,
        };
        // 50° snaps to 45°.
        let to = p(50f64.to_radians().cos(), 50f64.to_radians().sin());
        let snapped = drag_delta(&rotate, to, true).unwrap();
        assert!(snapped.approx_eq(&Transform::rotate(45f64.to_radians()), 1e-9));
        let free = drag_delta(&rotate, to, false).unwrap();
        assert!(free.approx_eq(&Transform::rotate(50f64.to_radians()), 1e-9));

        let bounds = Rect::new(p(0.0, 0.0), p(20.0, 10.0));
        let scale = Interaction::Scale {
            handle: Handle::NE,
            start: p(20.0, 10.0),
            bounds,
        };
        let kept = drag_delta(&scale, p(40.0, 11.0), true).unwrap();
        let r = kept.transform_rect(&bounds);
        assert!((r.width() / r.height() - 2.0).abs() < 1e-9);
        assert_eq!(drag_delta(&Interaction::Idle, p(1.0, 1.0), true), None);
    }

    #[test]
    fn cursor_reflects_handles_and_drags() {
        let (mut state, a, _) = state_with_squares();
        let screen = |x, y| Some(state.viewport.to_screen(p(x, y)));
        assert_eq!(cursor_for(&state, screen(15.0, 15.0)), None);
        state.selection = vec![a];
        assert_eq!(
            cursor_for(&state, screen(20.0, 15.0)),
            Some(CursorIcon::ResizeHorizontal)
        );
        assert_eq!(
            cursor_for(&state, screen(14.0, 14.0)),
            Some(CursorIcon::Move)
        );
        state.interaction = Interaction::Move { start: p(0.0, 0.0) };
        assert_eq!(cursor_for(&state, None), Some(CursorIcon::Grabbing));
    }

    /// CPU side of the spec's 60 FPS target: a full egui frame of the canvas
    /// plus tessellation, panning every frame. GPU upload is not measured.
    #[test]
    #[ignore = "perf check: cargo test -p pf-app --release -- --ignored --nocapture"]
    fn canvas_frame_with_10k_segments_fits_the_60_fps_budget() {
        use std::time::{Duration, Instant};

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../pf-io/tests/fixtures/perf_10k.svg"
        );
        let bytes = std::fs::read(path).unwrap();
        let svg = pf_io::import_svg(&bytes, &pf_io::SvgImportOptions::default()).unwrap();
        let mut state = AppState::new();
        let command = Command::ImportDocument(svg.document);
        state.history.apply(&mut state.document, command).unwrap();

        let ctx = egui::Context::default();
        let frame = |state: &mut AppState| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| show_canvas(ui, state));
            });
            // No renderer here to upload font textures to.
            output.textures_delta.clear();
            ctx.tessellate(output.shapes, output.pixels_per_point)
        };
        let time_frames = |state: &mut AppState, label: &str| {
            // Warm-up builds the display cache and fits the view.
            frame(state);
            let frames = 120;
            let start = Instant::now();
            for _ in 0..frames {
                state.viewport.pan(Vec2::new(1.0, 0.5));
                assert!(!frame(state).is_empty());
            }
            let per_frame = start.elapsed() / frames;
            println!("canvas frame with 10k segments, {label}: {per_frame:?}");
            assert!(per_frame < Duration::from_micros(16_667));
        };

        time_frames(&mut state, "nothing selected");
        actions::select_all(&mut state);
        state.interaction = Interaction::Move {
            start: Point::ORIGIN,
        };
        time_frames(&mut state, "all selected, moving");
        state.show_preview = true;
        time_frames(&mut state, "all selected, moving, preview overlay");
    }

    #[test]
    fn canvas_renders_headless_with_the_preview_overlay() {
        let (mut state, a, _) = state_with_squares();
        state.show_preview = true;
        state.selection = vec![a];
        let revision = state.history.revision();
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| show_canvas(ui, &mut state));
            });
            // No renderer here to upload font textures to.
            output.textures_delta.clear();
            assert!(!output.shapes.is_empty());
        }
        let preview = state.preview.result().unwrap().as_ref().unwrap();
        assert_eq!(preview.cuts.len(), 2);
        // Drawing never edits the document.
        assert_eq!(state.history.revision(), revision);
    }

    #[test]
    fn the_laser_head_is_drawn_only_when_its_position_is_known() {
        let ctx = egui::Context::default();
        let shapes_drawn = |state: &mut AppState| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| show_canvas(ui, state));
            });
            // No renderer here to upload font textures to.
            output.textures_delta.clear();
            output.shapes.len()
        };
        let mut state = AppState::new();
        let without = shapes_drawn(&mut state);
        let report = pf_device::parse_status("<Idle|MPos:50.000,40.000,0.000|FS:0,0>").unwrap();
        let positions = report.positions(None);
        state
            .device
            .snapshot
            .apply(&pf_device::DeviceEvent::Status { report, positions });
        // A circle and two lines.
        assert_eq!(shapes_drawn(&mut state), without + 3);
    }

    #[test]
    fn grid_lines_cover_the_range_and_mark_majors() {
        let lines: Vec<(f64, bool)> = grid_lines(-3.0, 25.0, 5.0).collect();
        let xs: Vec<f64> = lines.iter().map(|l| l.0).collect();
        assert_eq!(xs, vec![0.0, 5.0, 10.0, 15.0, 20.0, 25.0]);
        assert!(lines[0].1);
        assert!(!lines[1].1);
    }
}
