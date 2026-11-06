//! Shape modifiers and their menu controls.

use std::collections::HashSet;

use egui::Ui;
use egui::{Color32, Pos2, Sense, Stroke, StrokeKind, vec2};
use geo::algorithm::bool_ops::{BooleanOps, FillRule, OpType};
use geo::algorithm::buffer::{Buffer, BufferStyle, LineJoin};
use geo::{Coord, LineString, MultiPolygon, Polygon};
use pf_core::{
    Command, Path, Point, Segment, Shape, ShapeId, ShapeKind, SubPath, Transform, Vec2,
    flatten_path,
};

use crate::actions;
use crate::state::{
    AppState, CanvasTool, NodeRef, OffsetCornerStyle, OffsetDirection, StatusLevel,
};

const FLATTEN_TOLERANCE_MM: f64 = 0.03;

/// Compact one-click modifier tools displayed underneath the canvas tools.
/// Modifier values and array dimensions remain editable in the top menu.
pub fn show_toolbar(ui: &mut Ui, state: &mut AppState) {
    ui.add_space(3.0);
    ui.separator();
    let items = [
        (
            ToolbarModifier::Offset,
            "Offset: select one or more closed shapes, click this icon to open settings, choose distance and direction, then click Create offset.",
        ),
        (
            ToolbarModifier::Weld,
            "Weld: select two or more closed shapes, then click to merge their outlines into one shape.",
        ),
        (
            ToolbarModifier::Union,
            "Boolean union: select exactly two closed shapes to combine them.",
        ),
        (
            ToolbarModifier::Subtract,
            "Boolean subtract: select two shapes; the first selected shape is reduced by the second.",
        ),
        (
            ToolbarModifier::Intersect,
            "Boolean intersect: select exactly two shapes to keep only their overlapping area.",
        ),
        (
            ToolbarModifier::RectArray,
            "Rectangular array: select objects, set rows, columns, and gaps in Modifiers, then click to create copies.",
        ),
        (
            ToolbarModifier::CircularArray,
            "Circular array: select objects, choose the copy count in Modifiers, then click to arrange copies around their bounds center.",
        ),
        (
            ToolbarModifier::Round,
            "Round corners: click to activate, click each vertex to toggle it, then press Enter or click this tool again to apply. Esc cancels. Set radius in Modifiers.",
        ),
    ];
    for (tool, label) in items {
        let enabled = match tool {
            ToolbarModifier::Offset => true,
            ToolbarModifier::RectArray
            | ToolbarModifier::CircularArray
            | ToolbarModifier::Round => !state.selection.is_empty(),
            ToolbarModifier::Weld => state.selection.len() > 1,
            ToolbarModifier::Union | ToolbarModifier::Subtract | ToolbarModifier::Intersect => {
                state.selection.len() == 2
            }
        };
        let (rect, response) = ui.allocate_exact_size(vec2(34.0, 30.0), Sense::click());
        if tool == ToolbarModifier::Round && state.canvas_tool == CanvasTool::RoundCorners {
            ui.painter()
                .rect_filled(rect.shrink(1.0), 4.0, ui.visuals().selection.bg_fill);
        }
        let color = if enabled {
            if tool == ToolbarModifier::Round && state.canvas_tool == CanvasTool::RoundCorners {
                Color32::WHITE
            } else {
                ui.visuals().text_color()
            }
        } else {
            ui.visuals().weak_text_color()
        };
        draw_modifier_icon(ui.painter(), rect, tool, color);
        let response = response.on_hover_text(label);
        if matches!(tool, ToolbarModifier::Offset) {
            show_offset_popup(&response, state);
        }
        let clicked = response.clicked();
        if clicked && enabled {
            if !matches!(tool, ToolbarModifier::Round)
                && state.canvas_tool == CanvasTool::RoundCorners
            {
                state.canvas_tool = CanvasTool::Select;
                state.rounding_nodes.clear();
            }
            match tool {
                ToolbarModifier::Offset => {}
                ToolbarModifier::Weld => weld_selected(state),
                ToolbarModifier::Union => boolean_selected(state, BooleanKind::Union),
                ToolbarModifier::Subtract => boolean_selected(state, BooleanKind::Subtract),
                ToolbarModifier::Intersect => boolean_selected(state, BooleanKind::Intersect),
                ToolbarModifier::RectArray => rectangular_array(state),
                ToolbarModifier::CircularArray => circular_array(state),
                ToolbarModifier::Round => toggle_corner_rounding(state),
            }
        }
    }
    ui.add_space(2.0);
    ui.label("Radius:");
    ui.add_sized(
        vec2(52.0, 20.0),
        egui::DragValue::new(&mut state.modifier_corner_radius_mm)
            .speed(0.1)
            .range(0.1..=100.0)
            .fixed_decimals(1)
            .suffix(" mm"),
    )
    .on_hover_text("Corner radius in millimeters. Click the Round Corners icon, select vertices, then press Enter to apply this radius.");
}

fn show_offset_popup(response: &egui::Response, state: &mut AppState) {
    let popup_id = response.id.with("offset_settings_popup");
    egui::Popup::menu(response)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(210.0);
            ui.heading("Offset shapes");
            ui.label("Creates copies of the selected closed shapes.");
            ui.horizontal(|ui| {
                ui.label("Distance");
                ui.add(
                    egui::DragValue::new(&mut state.modifier_offset_mm)
                        .speed(0.25)
                        .range(0.1..=1000.0)
                        .fixed_decimals(1)
                        .suffix(" mm"),
                );
            });
            ui.label("Direction");
            ui.radio_value(
                &mut state.modifier_offset_direction,
                OffsetDirection::Outward,
                "Outward",
            );
            ui.radio_value(
                &mut state.modifier_offset_direction,
                OffsetDirection::Inward,
                "Inward",
            );
            ui.radio_value(
                &mut state.modifier_offset_direction,
                OffsetDirection::Both,
                "Both",
            );
            ui.separator();
            ui.label("Corner style");
            ui.radio_value(
                &mut state.modifier_offset_corner_style,
                OffsetCornerStyle::Rounded,
                "Rounded",
            );
            ui.radio_value(
                &mut state.modifier_offset_corner_style,
                OffsetCornerStyle::Sharp,
                "Sharp",
            );
            ui.separator();
            let apply = ui.add_enabled(
                !state.selection.is_empty(),
                egui::Button::new("Create offset"),
            );
            if apply.clicked() {
                offset_selected(state);
                egui::Popup::close_id(ui.ctx(), popup_id);
            }
        });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolbarModifier {
    Offset,
    Weld,
    Union,
    Subtract,
    Intersect,
    RectArray,
    CircularArray,
    Round,
}

fn draw_modifier_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    tool: ToolbarModifier,
    color: Color32,
) {
    let center = rect.center();
    let stroke = Stroke::new(1.5, color);
    let p = |x: f32, y: f32| Pos2::new(center.x + x, center.y + y);
    match tool {
        ToolbarModifier::Offset => {
            painter.circle_stroke(center, 8.0, stroke);
            painter.circle_stroke(center, 4.5, stroke);
        }
        ToolbarModifier::Weld
        | ToolbarModifier::Union
        | ToolbarModifier::Subtract
        | ToolbarModifier::Intersect => {
            painter.circle_stroke(p(-3.0, 0.0), 6.0, stroke);
            painter.circle_stroke(p(3.0, 0.0), 6.0, stroke);
            if matches!(tool, ToolbarModifier::Intersect) {
                painter.circle_filled(center, 2.0, color);
            }
            if matches!(tool, ToolbarModifier::Subtract) {
                painter.line_segment([p(-7.0, 0.0), p(-1.0, 0.0)], Stroke::new(2.5, color));
            }
            if matches!(tool, ToolbarModifier::Weld | ToolbarModifier::Union) {
                painter.line_segment([p(-1.0, -7.0), p(1.0, -7.0)], stroke);
            }
        }
        ToolbarModifier::RectArray => {
            for row in 0..3 {
                for column in 0..3 {
                    painter.rect_filled(
                        egui::Rect::from_center_size(
                            p(-6.0 + column as f32 * 6.0, -6.0 + row as f32 * 6.0),
                            vec2(3.0, 3.0),
                        ),
                        0.3,
                        color,
                    );
                }
            }
        }
        ToolbarModifier::CircularArray => {
            painter.circle_stroke(center, 7.0, stroke);
            for index in 0..6 {
                let angle = std::f32::consts::TAU * index as f32 / 6.0;
                painter.circle_filled(p(7.0 * angle.cos(), 7.0 * angle.sin()), 1.5, color);
            }
            painter.circle_filled(center, 2.0, color);
        }
        ToolbarModifier::Round => {
            painter.rect_stroke(
                egui::Rect::from_center_size(center, vec2(16.0, 16.0)),
                4.0,
                stroke,
                StrokeKind::Middle,
            );
            painter.line_segment([p(2.0, 8.0), p(8.0, 8.0)], Stroke::new(2.5, color));
        }
    }
}

pub fn show_menu(ui: &mut Ui, state: &mut AppState) {
    let selected = state.selection.len();
    ui.label(format!("{selected} selected"));
    ui.separator();

    ui.horizontal(|ui| {
        ui.label("Offset");
        ui.add(
            egui::DragValue::new(&mut state.modifier_offset_mm)
                .speed(0.25)
                .suffix(" mm"),
        );
    });
    ui.horizontal(|ui| {
        ui.label("Direction");
        ui.radio_value(
            &mut state.modifier_offset_direction,
            OffsetDirection::Outward,
            "Outward",
        );
        ui.radio_value(
            &mut state.modifier_offset_direction,
            OffsetDirection::Inward,
            "Inward",
        );
        ui.radio_value(
            &mut state.modifier_offset_direction,
            OffsetDirection::Both,
            "Both",
        );
    });
    ui.horizontal(|ui| {
        ui.label("Corner style");
        ui.radio_value(
            &mut state.modifier_offset_corner_style,
            OffsetCornerStyle::Rounded,
            "Rounded",
        );
        ui.radio_value(
            &mut state.modifier_offset_corner_style,
            OffsetCornerStyle::Sharp,
            "Sharp",
        );
    });
    if ui
        .add_enabled(selected > 0, egui::Button::new("Offset selected shapes"))
        .on_hover_text("Create an offset copy of every selected closed shape. Positive distance expands outward; negative buffer is used for inward offsets.")
        .clicked()
    {
        offset_selected(state);
    }

    if ui
        .add_enabled(selected > 1, egui::Button::new("Weld all selected shapes"))
        .clicked()
    {
        weld_selected(state);
    }

    ui.menu_button("Boolean (select exactly 2)", |ui| {
        let enabled = selected == 2;
        if ui
            .add_enabled(enabled, egui::Button::new("Union"))
            .clicked()
        {
            boolean_selected(state, BooleanKind::Union);
            ui.close();
        }
        if ui
            .add_enabled(enabled, egui::Button::new("Subtract (first minus second)"))
            .clicked()
        {
            boolean_selected(state, BooleanKind::Subtract);
            ui.close();
        }
        if ui
            .add_enabled(enabled, egui::Button::new("Intersect"))
            .clicked()
        {
            boolean_selected(state, BooleanKind::Intersect);
            ui.close();
        }
    });

    ui.menu_button("Rectangular array", |ui| {
        ui.horizontal(|ui| {
            ui.label("Columns");
            ui.add(egui::DragValue::new(&mut state.modifier_array_columns).range(1..=100));
            ui.label("Rows");
            ui.add(egui::DragValue::new(&mut state.modifier_array_rows).range(1..=100));
        });
        ui.horizontal(|ui| {
            ui.label("X spacing");
            ui.add(
                egui::DragValue::new(&mut state.modifier_array_spacing_x_mm)
                    .speed(0.5)
                    .suffix(" mm"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Y spacing");
            ui.add(
                egui::DragValue::new(&mut state.modifier_array_spacing_y_mm)
                    .speed(0.5)
                    .suffix(" mm"),
            );
        });
        if ui
            .add_enabled(selected > 0, egui::Button::new("Create array"))
            .clicked()
        {
            rectangular_array(state);
            ui.close();
        }
    });

    ui.menu_button("Circular array", |ui| {
        ui.horizontal(|ui| {
            ui.label("Copies, including original");
            ui.add(egui::DragValue::new(&mut state.modifier_circular_count).range(2..=360));
        });
        ui.label("Center: center of selected bounds");
        if ui
            .add_enabled(selected > 0, egui::Button::new("Create circular array"))
            .clicked()
        {
            circular_array(state);
            ui.close();
        }
    });

    ui.separator();
    ui.horizontal(|ui| {
        ui.label("Corner radius");
        ui.add(
            egui::DragValue::new(&mut state.modifier_corner_radius_mm)
                .speed(0.25)
                .suffix(" mm"),
        );
    });
    if ui
        .add_enabled(selected > 0, egui::Button::new(if state.canvas_tool == crate::state::CanvasTool::RoundCorners { "Apply to chosen corners (Enter)" } else { "Choose corners to round" }))
        .on_hover_text("Activate, click each corner to toggle it, then press Enter or click this again to round the chosen corners. Esc cancels.")
        .clicked()
    {
        toggle_corner_rounding(state);
        ui.close();
    }
}

#[derive(Clone, Copy)]
enum BooleanKind {
    Union,
    Subtract,
    Intersect,
}

fn offset_selected(state: &mut AppState) {
    if !state.modifier_offset_mm.is_finite() || state.modifier_offset_mm.abs() < 1e-9 {
        state.set_status(
            StatusLevel::Warning,
            "Offset distance must be a non-zero finite value",
        );
        return;
    }
    let shapes = selected_shapes(state);
    let mut commands = Vec::new();
    let directions = match state.modifier_offset_direction {
        OffsetDirection::Outward => &[1.0][..],
        OffsetDirection::Inward => &[-1.0][..],
        OffsetDirection::Both => &[1.0, -1.0][..],
    };
    for shape in &shapes {
        let Ok(geometry) = geometry_from_shape(shape) else {
            state.set_status(
                StatusLevel::Error,
                "Offset needs closed, valid shape outlines",
            );
            return;
        };
        let mut produced = false;
        for direction in directions {
            let distance = state.modifier_offset_mm.abs() * *direction;
            let result = match state.modifier_offset_corner_style {
                OffsetCornerStyle::Rounded => geometry.buffer(distance),
                OffsetCornerStyle::Sharp => geometry
                    // Keep the minimum miter angle small so acute corners remain sharp.
                    .buffer_with_style(BufferStyle::new(distance).line_join(LineJoin::Miter(0.05))),
            };
            let path = path_from_geometry(&result);
            if path.is_empty() {
                continue;
            }
            produced = true;
            commands.push(Command::CreateShape {
                kind: ShapeKind::Path(path),
                transform: Transform::identity(),
                layer: Some(shape.layer_id),
            });
        }
        if !produced {
            state.set_status(StatusLevel::Warning, "Offset produced no geometry");
            return;
        }
    }
    let first_new = state.document.shapes().len();
    apply_batch(state, "Offset shapes", commands);
    if state.document.shapes().len() > first_new {
        state.selection = state.document.shapes()[first_new..]
            .iter()
            .map(|shape| shape.id)
            .collect();
        state.selected_layer = state
            .selection
            .first()
            .and_then(|id| state.document.shape(*id))
            .map(|s| s.layer_id);
    }
}

fn weld_selected(state: &mut AppState) {
    let shapes = selected_shapes(state);
    if shapes.len() < 2 {
        return;
    }
    let mut result = match geometry_from_shape(&shapes[0]) {
        Ok(geometry) => geometry,
        Err(_) => {
            return state.set_status(
                StatusLevel::Error,
                "Weld needs closed, valid shape outlines",
            );
        }
    };
    for shape in &shapes[1..] {
        let next = match geometry_from_shape(shape) {
            Ok(geometry) => geometry,
            Err(_) => {
                return state.set_status(
                    StatusLevel::Error,
                    "Weld needs closed, valid shape outlines",
                );
            }
        };
        result = result.boolean_op_with_fill_rule(&next, OpType::Union, FillRule::EvenOdd);
    }
    replace_with_result(state, &shapes, result, "Weld shapes");
}

fn boolean_selected(state: &mut AppState, operation: BooleanKind) {
    let shapes = selected_shapes(state);
    if shapes.len() != 2 {
        return;
    }
    let (first, second) = match (
        geometry_from_shape(&shapes[0]),
        geometry_from_shape(&shapes[1]),
    ) {
        (Ok(first), Ok(second)) => (first, second),
        _ => {
            return state.set_status(
                StatusLevel::Error,
                "Boolean operations need closed, valid shape outlines",
            );
        }
    };
    let result = match operation {
        BooleanKind::Union => {
            first.boolean_op_with_fill_rule(&second, OpType::Union, FillRule::EvenOdd)
        }
        BooleanKind::Subtract => {
            first.boolean_op_with_fill_rule(&second, OpType::Difference, FillRule::EvenOdd)
        }
        BooleanKind::Intersect => {
            first.boolean_op_with_fill_rule(&second, OpType::Intersection, FillRule::EvenOdd)
        }
    };
    let label = match operation {
        BooleanKind::Union => "Boolean union",
        BooleanKind::Subtract => "Boolean subtract",
        BooleanKind::Intersect => "Boolean intersect",
    };
    replace_with_result(state, &shapes, result, label);
}

fn replace_with_result(
    state: &mut AppState,
    shapes: &[Shape],
    result: MultiPolygon<f64>,
    label: &'static str,
) {
    let ids: Vec<ShapeId> = shapes.iter().map(|shape| shape.id).collect();
    let mut commands = Vec::new();
    if let Some(primary) = shapes.first()
        && !result.0.is_empty()
    {
        commands.push(Command::SetShapeGeometry {
            id: primary.id,
            kind: ShapeKind::Path(path_from_geometry(&result)),
            transform: Transform::identity(),
        });
    }
    let deleted = if result.0.is_empty() {
        ids.as_slice()
    } else {
        &ids[1..]
    };
    if !deleted.is_empty() {
        commands.push(Command::DeleteShapes(deleted.to_vec()));
    }
    let primary_id = shapes.first().map(|shape| shape.id);
    let primary_layer = shapes.first().map(|shape| shape.layer_id);
    let revision = state.history.revision();
    apply_batch(state, label, commands);
    if state.history.revision() == revision {
        return;
    }
    state.selection = if result.0.is_empty() {
        Vec::new()
    } else {
        primary_id.into_iter().collect()
    };
    state.selected_layer = primary_layer;
}

fn rectangular_array(state: &mut AppState) {
    let shapes = selected_shapes(state);
    if shapes.is_empty() {
        return;
    }
    let original_ids: Vec<ShapeId> = shapes.iter().map(|shape| shape.id).collect();
    let (columns, rows) = (
        state.modifier_array_columns.clamp(1, 100),
        state.modifier_array_rows.clamp(1, 100),
    );
    let (dx, dy) = (
        state.modifier_array_spacing_x_mm,
        state.modifier_array_spacing_y_mm,
    );
    if !(dx.is_finite() && dy.is_finite()) {
        state.set_status(StatusLevel::Warning, "Array spacing must be finite");
        return;
    }
    let Some(bounds) = shapes
        .iter()
        .filter_map(Shape::bounds)
        .reduce(|a, b| a.union(&b))
    else {
        return;
    };
    let step_x = bounds.width() + dx;
    let step_y = bounds.height() + dy;
    let mut commands = Vec::new();
    for row in 0..rows {
        for column in 0..columns {
            if row == 0 && column == 0 {
                continue;
            }
            for shape in &shapes {
                commands.push(Command::CreateShape {
                    kind: shape.kind.clone(),
                    transform: shape.transform.then(Transform::translate(
                        column as f64 * step_x,
                        row as f64 * step_y,
                    )),
                    layer: Some(shape.layer_id),
                });
            }
        }
    }
    let first_new = state.document.shapes().len();
    apply_batch(state, "Rectangular array", commands);
    select_array_shapes(state, original_ids, first_new);
}

fn circular_array(state: &mut AppState) {
    let shapes = selected_shapes(state);
    if shapes.is_empty() {
        return;
    }
    let original_ids: Vec<ShapeId> = shapes.iter().map(|shape| shape.id).collect();
    let count = state.modifier_circular_count.clamp(2, 360);
    let Some(bounds) = shapes
        .iter()
        .filter_map(|shape| shape.bounds())
        .reduce(|a, b| a.union(&b))
    else {
        return;
    };
    let center = Point::new(
        (bounds.min.x + bounds.max.x) * 0.5,
        (bounds.min.y + bounds.max.y) * 0.5,
    );
    let mut commands = Vec::new();
    for index in 1..count {
        let rotation =
            Transform::rotate_about(center, std::f64::consts::TAU * index as f64 / count as f64);
        for shape in &shapes {
            commands.push(Command::CreateShape {
                kind: shape.kind.clone(),
                transform: shape.transform.then(rotation),
                layer: Some(shape.layer_id),
            });
        }
    }
    let first_new = state.document.shapes().len();
    apply_batch(state, "Circular array", commands);
    select_array_shapes(state, original_ids, first_new);
}

pub fn toggle_corner_rounding(state: &mut AppState) {
    if state.canvas_tool == CanvasTool::RoundCorners {
        apply_selected_corners(state);
        state.canvas_tool = CanvasTool::Select;
        state.rounding_nodes.clear();
    } else {
        state.canvas_tool = CanvasTool::RoundCorners;
        state.rounding_nodes.clear();
        state.selected_node = None;
        state.selected_nodes.clear();
        state.selected_segment = None;
        state.selected_segment_point = None;
        state.set_status(
            StatusLevel::Info,
            "Click the selected shape corners to toggle them, then press Enter or click the rounding tool again. Esc cancels.",
        );
    }
}

pub fn apply_selected_corners(state: &mut AppState) {
    let nodes = state.rounding_nodes.clone();
    if nodes.is_empty() {
        state.set_status(StatusLevel::Warning, "Select one or more corners first");
        return;
    }
    round_selected(state, &nodes);
}

fn round_selected(state: &mut AppState, selected_nodes: &[NodeRef]) {
    let radius = state.modifier_corner_radius_mm;
    if !radius.is_finite() || radius <= 0.0 {
        state.set_status(
            StatusLevel::Warning,
            "Corner radius must be greater than zero",
        );
        return;
    }
    let shapes = selected_shapes(state);
    let mut commands = Vec::new();
    for shape in &shapes {
        if !selected_nodes.iter().any(|node| node.shape_id == shape.id) {
            continue;
        }
        let path = shape.world_path();
        if path
            .subpaths()
            .iter()
            .flat_map(|subpath| subpath.segments())
            .any(|segment| !matches!(segment, Segment::Line { .. }))
        {
            state.set_status(
                StatusLevel::Error,
                "Rounding currently supports straight line paths; convert curves to lines first",
            );
            return;
        }
        let flattened = match flatten_path(&path, FLATTEN_TOLERANCE_MM) {
            Ok(paths) => paths,
            Err(error) => {
                return state
                    .set_status(StatusLevel::Error, format!("Could not read path: {error}"));
            }
        };
        let mut subpaths = Vec::with_capacity(flattened.len());
        for (subpath_index, polyline) in flattened.iter().enumerate() {
            let anchors: HashSet<usize> = selected_nodes
                .iter()
                .filter(|node| node.shape_id == shape.id && node.subpath == subpath_index)
                .map(|node| node.anchor)
                .collect();
            if anchors.is_empty() {
                if let Some(subpath) = path.subpaths().get(subpath_index) {
                    subpaths.push(subpath.clone());
                }
            } else if let Some(rounded) =
                fillet_polyline(&polyline.points, polyline.closed, radius, &anchors)
            {
                subpaths.push(rounded);
            }
        }
        let rounded = Path::new(subpaths);
        if rounded.is_empty() {
            state.set_status(
                StatusLevel::Warning,
                "No corners could be rounded on the selected shapes",
            );
            return;
        }
        commands.push(Command::SetShapeGeometry {
            id: shape.id,
            kind: ShapeKind::Path(rounded),
            transform: Transform::identity(),
        });
    }
    apply_batch(state, "Round corners", commands);
}

fn selected_shapes(state: &AppState) -> Vec<Shape> {
    state
        .selection
        .iter()
        .filter_map(|id| state.document.shape(*id).cloned())
        .collect()
}

fn geometry_from_shape(shape: &Shape) -> Result<MultiPolygon<f64>, ()> {
    let flattened = flatten_path(&shape.world_path(), FLATTEN_TOLERANCE_MM).map_err(|_| ())?;
    if flattened.is_empty()
        || flattened
            .iter()
            .any(|path| !path.closed || path.points.len() < 4)
    {
        return Err(());
    }
    let mut filled = MultiPolygon(Vec::new());
    for polyline in flattened {
        let ring = LineString(
            polyline
                .points
                .into_iter()
                .map(|point| Coord {
                    x: point.x,
                    y: point.y,
                })
                .collect(),
        );
        let contour = MultiPolygon(vec![Polygon::new(ring, Vec::new())]);
        // SVG and PhotonForge paths use even-odd fill semantics. Folding
        // contours one at a time preserves holes and resolves overlaps.
        filled = filled.boolean_op_with_fill_rule(&contour, OpType::Xor, FillRule::EvenOdd);
    }
    Ok(filled)
}

fn path_from_geometry(geometry: &MultiPolygon<f64>) -> Path {
    let mut subpaths = Vec::new();
    for polygon in &geometry.0 {
        append_ring(&mut subpaths, polygon.exterior());
        for ring in polygon.interiors() {
            append_ring(&mut subpaths, ring);
        }
    }
    Path::new(subpaths)
}

fn append_ring(subpaths: &mut Vec<SubPath>, ring: &LineString<f64>) {
    let points: Vec<Point> = ring
        .0
        .iter()
        .map(|coordinate| Point::new(coordinate.x, coordinate.y))
        .collect();
    if points.len() < 4 {
        return;
    }
    let segments = points
        .windows(2)
        .map(|pair| Segment::Line {
            from: pair[0],
            to: pair[1],
        })
        .collect();
    if let Ok(path) = SubPath::new(segments, true) {
        subpaths.push(path);
    }
}

fn fillet_polyline(
    points: &[Point],
    closed: bool,
    radius: f64,
    selected_anchors: &HashSet<usize>,
) -> Option<SubPath> {
    let mut vertices = points.to_vec();
    if closed && vertices.len() > 1 && vertices.first() == vertices.last() {
        vertices.pop();
    }
    let minimum = if closed { 3 } else { 2 };
    if vertices.len() < minimum {
        return None;
    }
    let n = vertices.len();
    // incoming tangent, outgoing tangent, circle center, start angle, signed turn
    let mut corners: Vec<Option<(Point, Point, Point, f64, f64)>> = vec![None; n];
    for index in 0..n {
        if !closed && (index == 0 || index + 1 == n) {
            continue;
        }
        if !selected_anchors.contains(&index) {
            continue;
        }
        let previous = vertices[(index + n - 1) % n];
        let vertex = vertices[index];
        let next = vertices[(index + 1) % n];
        let incoming = (vertex - previous).normalize()?;
        let outgoing = (next - vertex).normalize()?;
        let turn = incoming.cross(outgoing).atan2(incoming.dot(outgoing));
        let half_tangent = (turn.abs() * 0.5).tan();
        if half_tangent.abs() < 1e-8
            || turn.abs() < 1e-6
            || (std::f64::consts::PI - turn.abs()).abs() < 1e-6
        {
            continue;
        }
        let max_distance = (vertex.distance(previous).min(vertex.distance(next)) * 0.49).max(0.0);
        let tangent_distance = (radius * half_tangent).min(max_distance);
        let actual_radius = tangent_distance / half_tangent;
        let incoming_point = vertex - incoming * tangent_distance;
        let outgoing_point = vertex + outgoing * tangent_distance;
        let normal = Vec2::new(-incoming.y, incoming.x) * turn.signum();
        let center = incoming_point + normal * actual_radius;
        let start_angle = (incoming_point.y - center.y).atan2(incoming_point.x - center.x);
        corners[index] = Some((incoming_point, outgoing_point, center, start_angle, turn));
    }

    let mut segments = Vec::new();
    if closed {
        for index in 0..n {
            let previous_index = (index + n - 1) % n;
            let from = corners[previous_index]
                .as_ref()
                .map(|corner| corner.1)
                .unwrap_or(vertices[previous_index]);
            let to = corners[index]
                .as_ref()
                .map(|corner| corner.0)
                .unwrap_or(vertices[index]);
            if from.distance(to) > 1e-9 {
                segments.push(Segment::Line { from, to });
            }
            if let Some((incoming, end, center, start_angle, turn)) = corners[index] {
                let radius = incoming.distance(center);
                let end_angle = (end.y - center.y).atan2(end.x - center.x);
                let sweep = signed_sweep(start_angle, end_angle, turn.signum());
                segments.push(Segment::Arc {
                    center,
                    radii: Vec2::new(radius, radius),
                    x_rotation: 0.0,
                    start_angle,
                    sweep_angle: sweep,
                });
            }
        }
    } else {
        let first = vertices[0];
        let last = vertices[n - 1];
        let first_out = corners[1]
            .as_ref()
            .map(|corner| corner.0)
            .unwrap_or(vertices[1]);
        if first.distance(first_out) > 1e-9 {
            segments.push(Segment::Line {
                from: first,
                to: first_out,
            });
        }
        for index in 1..n - 1 {
            let incoming = corners[index]
                .as_ref()
                .map(|corner| corner.0)
                .unwrap_or(vertices[index]);
            let outgoing = corners[index]
                .as_ref()
                .map(|corner| corner.1)
                .unwrap_or(vertices[index]);
            if let Some(previous) = segments.last().map(Segment::end)
                && previous.distance(incoming) > 1e-9
            {
                segments.push(Segment::Line {
                    from: previous,
                    to: incoming,
                });
            }
            if let Some((_, _, center, start_angle, turn)) = corners[index] {
                let radius = (incoming - center).length();
                let end_angle = (outgoing.y - center.y).atan2(outgoing.x - center.x);
                segments.push(Segment::Arc {
                    center,
                    radii: Vec2::new(radius, radius),
                    x_rotation: 0.0,
                    start_angle,
                    sweep_angle: signed_sweep(start_angle, end_angle, turn.signum()),
                });
            }
        }
        let last_in = corners[n - 2]
            .as_ref()
            .map(|corner| corner.1)
            .unwrap_or(vertices[n - 2]);
        if let Some(previous) = segments.last().map(Segment::end)
            && previous.distance(last_in) > 1e-9
        {
            segments.push(Segment::Line {
                from: previous,
                to: last_in,
            });
        }
        if last_in.distance(last) > 1e-9 {
            segments.push(Segment::Line {
                from: last_in,
                to: last,
            });
        }
    }
    SubPath::new(segments, closed).ok()
}

fn signed_sweep(start: f64, end: f64, direction: f64) -> f64 {
    let mut sweep = end - start;
    if direction > 0.0 {
        while sweep <= 0.0 {
            sweep += std::f64::consts::TAU;
        }
    } else {
        while sweep >= 0.0 {
            sweep -= std::f64::consts::TAU;
        }
    }
    sweep
}

fn apply_batch(state: &mut AppState, label: &'static str, commands: Vec<Command>) {
    if commands.is_empty() {
        return;
    }
    actions::apply(state, Command::Batch { label, commands });
}

fn select_array_shapes(state: &mut AppState, original_ids: Vec<ShapeId>, first_new: usize) {
    if first_new >= state.document.shapes().len() {
        return;
    }
    state.selection = original_ids;
    state.selection.extend(
        state.document.shapes()[first_new..]
            .iter()
            .map(|shape| shape.id),
    );
}
