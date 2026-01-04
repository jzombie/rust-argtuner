use ratatui::prelude::{Constraint, Direction, Layout, Rect};

use crate::window::RegionMap;

#[derive(Debug, Clone)]
pub enum LayoutNode<Id: Copy + Eq + Ord> {
    Leaf(Id),
    Split {
        direction: Direction,
        constraints: Vec<Constraint>,
        children: Vec<LayoutNode<Id>>,
    },
}

impl<Id: Copy + Eq + Ord> LayoutNode<Id> {
    pub fn leaf(id: Id) -> Self {
        Self::Leaf(id)
    }

    pub fn split(
        direction: Direction,
        constraints: Vec<Constraint>,
        children: Vec<LayoutNode<Id>>,
    ) -> Self {
        Self::Split {
            direction,
            constraints,
            children,
        }
    }

    pub fn layout(&self, area: Rect) -> Vec<(Id, Rect)> {
        match self {
            LayoutNode::Leaf(id) => vec![(*id, area)],
            LayoutNode::Split {
                direction,
                constraints,
                children,
            } => {
                let splits = split_rects(*direction, constraints, area, children.len());
                let mut results = Vec::new();
                for (child, rect) in children.iter().zip(splits.into_iter()) {
                    results.extend(child.layout(rect));
                }
                results
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RectSpec {
    Absolute(Rect),
    Percent {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    },
}

impl RectSpec {
    pub fn resolve(self, area: Rect) -> Rect {
        match self {
            RectSpec::Absolute(rect) => rect,
            RectSpec::Percent {
                x,
                y,
                width,
                height,
            } => {
                let to_abs = |base: u16, pct: u16| (base as u32 * pct as u32 / 100) as u16;
                Rect {
                    x: area.x.saturating_add(to_abs(area.width, x)),
                    y: area.y.saturating_add(to_abs(area.height, y)),
                    width: to_abs(area.width, width),
                    height: to_abs(area.height, height),
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct FloatingPane<Id: Copy + Eq + Ord> {
    pub id: Id,
    pub rect: RectSpec,
}

#[derive(Debug, Clone)]
pub struct LayoutPlan<Id: Copy + Eq + Ord> {
    pub root: LayoutNode<Id>,
    pub floating: Vec<FloatingPane<Id>>,
}

impl<Id: Copy + Eq + Ord> LayoutPlan<Id> {
    pub fn new(root: LayoutNode<Id>) -> Self {
        Self {
            root,
            floating: Vec::new(),
        }
    }

    pub fn regions(&self, area: Rect) -> RegionMap<Id> {
        let mut regions = RegionMap::default();
        for (id, rect) in self.root.layout(area) {
            regions.set(id, rect);
        }
        for floating in &self.floating {
            regions.set(floating.id, floating.rect.resolve(area));
        }
        regions
    }
}

fn split_rects(
    direction: Direction,
    constraints: &[Constraint],
    area: Rect,
    child_count: usize,
) -> Vec<Rect> {
    let constraints = if constraints.is_empty() || constraints.len() != child_count {
        let count = child_count.max(1) as u16;
        vec![Constraint::Percentage(100 / count); child_count]
    } else {
        constraints.to_vec()
    };
    Layout::default()
        .direction(direction)
        .constraints(constraints)
        .split(area)
        .to_vec()
}
