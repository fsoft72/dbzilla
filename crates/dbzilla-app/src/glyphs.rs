//! Vector icons drawn with the painter on a 16-unit grid (Obsidian style, no icon font).

use eframe::egui::{pos2, vec2, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind};

/// Stroke width of icon lines.
const STROKE_WIDTH: f32 = 1.6;
/// Share of the smaller rect side used by the icon box.
const ICON_SCALE: f32 = 0.6;
/// Segments used to sample curves.
const CURVE_SEGMENTS: usize = 20;

/// All icons of the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ico {
    Search,
    Close,
    Plus,
    Minus,
    Database,
    Table,
    View,
    Function,
    Key,
    Lock,
    Eye,
    EyeOff,
    Copy,
    Refresh,
    Play,
    Stop,
    Commit,
    Discard,
    Preview,
    Settings,
    Folder,
    Filter,
    ChevronRight,
    ChevronDown,
    ChevronLeft,
    ChevronUp,
    Export,
    Import,
    Bolt,
    Trash,
    Edit,
    Plug,
    Info,
    Warning,
    Menu,
    Grip,
    File,
    History,
    Bookmark,
    Structure,
    Sidebar,
    Console,
    Shield,
}

/// Points of a quadratic curve.
fn quad(a: Pos2, c: Pos2, b: Pos2) -> Vec<Pos2> {
    (0..=CURVE_SEGMENTS)
        .map(|i| {
            let t = i as f32 / CURVE_SEGMENTS as f32;
            let m = 1.0 - t;
            pos2(m * m * a.x + 2.0 * m * t * c.x + t * t * b.x, m * m * a.y + 2.0 * m * t * c.y + t * t * b.y)
        })
        .collect()
}

/// Points of an elliptical arc from angle `a0` to `a1` (radians).
fn arc(center: Pos2, rx: f32, ry: f32, a0: f32, a1: f32) -> Vec<Pos2> {
    (0..=CURVE_SEGMENTS)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / CURVE_SEGMENTS as f32;
            pos2(center.x + rx * a.cos(), center.y + ry * a.sin())
        })
        .collect()
}

/// Paints `ico` centred in `rect` with `color`.
pub fn paint(p: &Painter, rect: Rect, ico: Ico, color: Color32) {
    let s = rect.width().min(rect.height()) * ICON_SCALE;
    let u = s / 16.0;
    let c = rect.center();
    let pt = |x: f32, y: f32| c + vec2(x - 8.0, y - 8.0) * u;
    let st = Stroke::new(STROKE_WIDTH, color);
    let line = |a: Pos2, b: Pos2| {
        p.line_segment([a, b], st);
    };
    let poly = |pts: &[(f32, f32)], closed: bool| {
        let v: Vec<Pos2> = pts.iter().map(|&(x, y)| pt(x, y)).collect();
        p.add(if closed { Shape::closed_line(v, st) } else { Shape::line(v, st) });
    };
    let curve = |pts: Vec<Pos2>| {
        p.add(Shape::line(pts, st));
    };
    let dot = |x: f32, y: f32, r: f32| {
        p.circle_filled(pt(x, y), r * u, color);
    };
    let ring = |x: f32, y: f32, r: f32| {
        p.circle_stroke(pt(x, y), r * u, st);
    };
    let rect_s = |x0: f32, y0: f32, x1: f32, y1: f32| {
        p.rect_stroke(Rect::from_two_pos(pt(x0, y0), pt(x1, y1)), 1.5 * u, st, StrokeKind::Middle);
    };
    use std::f32::consts::PI;
    match ico {
        Ico::Search => {
            ring(7.0, 7.0, 4.5);
            line(pt(10.5, 10.5), pt(14.0, 14.0));
        }
        Ico::Close => {
            line(pt(3.5, 3.5), pt(12.5, 12.5));
            line(pt(12.5, 3.5), pt(3.5, 12.5));
        }
        Ico::Plus => {
            line(pt(8.0, 3.0), pt(8.0, 13.0));
            line(pt(3.0, 8.0), pt(13.0, 8.0));
        }
        Ico::Minus => line(pt(3.0, 8.0), pt(13.0, 8.0)),
        Ico::Database => {
            let top = arc(pt(8.0, 4.0), 5.5 * u, 2.0 * u, 0.0, 2.0 * PI);
            curve(top);
            line(pt(2.5, 4.0), pt(2.5, 12.0));
            line(pt(13.5, 4.0), pt(13.5, 12.0));
            curve(arc(pt(8.0, 12.0), 5.5 * u, 2.0 * u, 0.0, PI));
            curve(arc(pt(8.0, 8.0), 5.5 * u, 2.0 * u, 0.0, PI));
        }
        Ico::Table => {
            rect_s(2.0, 3.0, 14.0, 13.0);
            line(pt(2.0, 6.5), pt(14.0, 6.5));
            line(pt(6.5, 3.0), pt(6.5, 13.0));
        }
        Ico::View | Ico::Eye | Ico::Preview => {
            curve(quad(pt(1.5, 8.0), pt(8.0, 1.5), pt(14.5, 8.0)));
            curve(quad(pt(1.5, 8.0), pt(8.0, 14.5), pt(14.5, 8.0)));
            ring(8.0, 8.0, 2.2);
        }
        Ico::EyeOff => {
            curve(quad(pt(1.5, 8.0), pt(8.0, 1.5), pt(14.5, 8.0)));
            curve(quad(pt(1.5, 8.0), pt(8.0, 14.5), pt(14.5, 8.0)));
            ring(8.0, 8.0, 2.2);
            line(pt(3.0, 13.0), pt(13.0, 3.0));
        }
        Ico::Function => {
            curve(quad(pt(10.5, 3.0), pt(8.0, 2.5), pt(8.0, 6.0)));
            line(pt(8.0, 6.0), pt(8.0, 11.0));
            curve(quad(pt(8.0, 11.0), pt(8.0, 13.5), pt(5.5, 13.0)));
            line(pt(5.5, 7.0), pt(10.5, 7.0));
        }
        Ico::Key => {
            ring(5.5, 10.5, 2.8);
            line(pt(7.5, 8.5), pt(13.5, 2.5));
            line(pt(11.5, 4.5), pt(13.0, 6.0));
        }
        Ico::Lock => {
            rect_s(3.5, 7.5, 12.5, 13.5);
            poly(&[(5.5, 7.5), (5.5, 5.5)], false);
            curve(arc(pt(8.0, 5.5), 2.5 * u, 2.5 * u, PI, 2.0 * PI));
            poly(&[(10.5, 5.5), (10.5, 7.5)], false);
            dot(8.0, 10.5, 1.0);
        }
        Ico::Copy => {
            poly(&[(6.0, 13.0), (13.0, 13.0), (13.0, 6.0), (6.0, 6.0)], true);
            poly(&[(10.0, 6.0), (10.0, 3.0), (3.0, 3.0), (3.0, 10.0), (6.0, 10.0)], false);
        }
        Ico::Refresh => {
            curve(arc(pt(8.0, 8.0), 5.0 * u, 5.0 * u, -0.3, 1.5 * PI));
            poly(&[(12.5, 2.5), (13.0, 6.5), (9.0, 6.0)], false);
        }
        Ico::Play => {
            let pts = vec![pt(5.0, 3.0), pt(13.0, 8.0), pt(5.0, 13.0)];
            p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        Ico::Stop => {
            p.rect_filled(Rect::from_two_pos(pt(4.0, 4.0), pt(12.0, 12.0)), 1.0 * u, color);
        }
        Ico::Commit => poly(&[(3.0, 8.5), (6.5, 12.0), (13.0, 4.5)], false),
        Ico::Discard => {
            poly(&[(6.0, 3.5), (3.0, 6.5), (6.0, 9.5)], false);
            poly(&[(3.0, 6.5), (9.5, 6.5), (13.0, 9.0), (9.5, 12.5), (6.0, 12.5)], false);
        }
        Ico::Settings => {
            for (y, k) in [(4.0, 10.0), (8.0, 5.0), (12.0, 11.0)] {
                line(pt(2.0, y), pt(14.0, y));
                p.circle_filled(pt(k, y), 1.9 * u, color);
            }
        }
        Ico::Folder => poly(&[(2.0, 4.5), (6.5, 4.5), (8.0, 6.5), (14.0, 6.5), (14.0, 12.5), (2.0, 12.5)], true),
        Ico::Filter => poly(&[(2.0, 3.0), (14.0, 3.0), (9.5, 8.5), (9.5, 13.0), (6.5, 11.5), (6.5, 8.5)], true),
        Ico::ChevronRight => poly(&[(6.0, 3.5), (10.5, 8.0), (6.0, 12.5)], false),
        Ico::ChevronLeft => poly(&[(10.0, 3.5), (5.5, 8.0), (10.0, 12.5)], false),
        Ico::ChevronDown => poly(&[(3.5, 6.0), (8.0, 10.5), (12.5, 6.0)], false),
        Ico::ChevronUp => poly(&[(3.5, 10.0), (8.0, 5.5), (12.5, 10.0)], false),
        Ico::Export => {
            line(pt(8.0, 10.0), pt(8.0, 2.5));
            poly(&[(5.0, 5.5), (8.0, 2.5), (11.0, 5.5)], false);
            poly(&[(3.0, 9.0), (3.0, 13.0), (13.0, 13.0), (13.0, 9.0)], false);
        }
        Ico::Import => {
            line(pt(8.0, 2.5), pt(8.0, 10.0));
            poly(&[(5.0, 7.0), (8.0, 10.0), (11.0, 7.0)], false);
            poly(&[(3.0, 9.0), (3.0, 13.0), (13.0, 13.0), (13.0, 9.0)], false);
        }
        Ico::Bolt => poly(&[(9.0, 2.0), (4.0, 9.0), (8.0, 9.0), (7.0, 14.0), (12.0, 7.0), (8.0, 7.0)], true),
        Ico::Trash => {
            line(pt(3.0, 4.5), pt(13.0, 4.5));
            poly(&[(6.5, 4.5), (6.5, 3.0), (9.5, 3.0), (9.5, 4.5)], false);
            poly(&[(4.5, 4.5), (5.0, 13.5), (11.0, 13.5), (11.5, 4.5)], false);
            line(pt(7.0, 7.0), pt(7.0, 11.0));
            line(pt(9.0, 7.0), pt(9.0, 11.0));
        }
        Ico::Edit => poly(&[(3.0, 13.0), (3.5, 10.0), (11.0, 2.5), (13.5, 5.0), (6.0, 12.5)], true),
        Ico::Plug => {
            rect_s(5.0, 7.0, 11.0, 11.0);
            line(pt(6.5, 4.0), pt(6.5, 7.0));
            line(pt(9.5, 4.0), pt(9.5, 7.0));
            line(pt(8.0, 11.0), pt(8.0, 14.0));
        }
        Ico::Info => {
            ring(8.0, 8.0, 6.0);
            dot(8.0, 5.0, 0.9);
            line(pt(8.0, 7.5), pt(8.0, 11.5));
        }
        Ico::Warning => {
            poly(&[(8.0, 2.5), (14.0, 13.0), (2.0, 13.0)], true);
            line(pt(8.0, 6.5), pt(8.0, 9.5));
            dot(8.0, 11.5, 0.9);
        }
        Ico::Menu => {
            for y in [4.0, 8.0, 12.0] {
                line(pt(2.5, y), pt(13.5, y));
            }
        }
        Ico::Grip => {
            for x in [5.5, 10.5] {
                for y in [4.0, 8.0, 12.0] {
                    dot(x, y, 1.1);
                }
            }
        }
        Ico::File => {
            poly(&[(4.0, 2.0), (10.0, 2.0), (13.0, 5.0), (13.0, 14.0), (4.0, 14.0)], true);
            line(pt(6.0, 8.0), pt(11.0, 8.0));
            line(pt(6.0, 11.0), pt(11.0, 11.0));
        }
        Ico::History => {
            ring(8.0, 8.0, 6.0);
            poly(&[(8.0, 4.5), (8.0, 8.0), (10.5, 9.5)], false);
        }
        Ico::Bookmark => poly(&[(4.0, 2.5), (12.0, 2.5), (12.0, 13.5), (8.0, 10.5), (4.0, 13.5)], true),
        Ico::Structure => {
            rect_s(2.5, 3.0, 13.5, 13.0);
            for y in [6.5, 9.5] {
                line(pt(5.0, y), pt(11.0, y));
            }
            dot(5.0, 11.5, 0.8);
        }
        Ico::Sidebar => {
            rect_s(2.0, 3.0, 14.0, 13.0);
            line(pt(6.0, 3.0), pt(6.0, 13.0));
        }
        Ico::Console => {
            rect_s(2.0, 3.0, 14.0, 13.0);
            poly(&[(4.5, 6.5), (7.0, 8.5), (4.5, 10.5)], false);
            line(pt(8.5, 10.5), pt(11.5, 10.5));
        }
        Ico::Shield => {
            poly(&[(8.0, 2.0), (13.0, 4.0), (13.0, 8.5), (8.0, 14.0), (3.0, 8.5), (3.0, 4.0)], true);
            poly(&[(5.8, 8.0), (7.5, 9.8), (10.5, 6.2)], false);
        }
    }
}
