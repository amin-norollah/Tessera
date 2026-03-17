//! A 2D mesh. Each router has five ports; a direction port carries a full-
//! duplex link to its neighbor, so [`Topology::adjacent`] is symmetric and
//! serves both output delivery and upstream crediting.

use tessera_core::{Coord, PortId, PortLabel, RouterId, Topology};

pub const EAST: u8 = 0;
pub const WEST: u8 = 1;
pub const NORTH: u8 = 2;
pub const SOUTH: u8 = 3;
pub const LOCAL: u8 = 4;

const PORTS: u8 = 5;

pub struct Mesh2D {
    width: u16,
    height: u16,
}

impl Mesh2D {
    pub fn new(width: u16, height: u16) -> Self {
        assert!(width >= 1 && height >= 1, "mesh dimensions must be >= 1");
        Self { width, height }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    #[inline]
    fn id(&self, x: u16, y: u16) -> RouterId {
        RouterId(y as u32 * self.width as u32 + x as u32)
    }
}

impl Topology for Mesh2D {
    fn num_routers(&self) -> u32 {
        self.width as u32 * self.height as u32
    }

    fn port_count(&self) -> u8 {
        PORTS
    }

    fn local_port(&self) -> PortId {
        PortId(LOCAL)
    }

    fn coord(&self, r: RouterId) -> Coord {
        let w = self.width as u32;
        Coord::new((r.0 % w) as u16, (r.0 / w) as u16, 0)
    }

    fn router_at(&self, c: Coord) -> Option<RouterId> {
        if c.z == 0 && c.x < self.width && c.y < self.height {
            Some(self.id(c.x, c.y))
        } else {
            None
        }
    }

    fn adjacent(&self, r: RouterId, p: PortId) -> Option<(RouterId, PortId)> {
        let c = self.coord(r);
        match p.0 {
            EAST if c.x + 1 < self.width => Some((self.id(c.x + 1, c.y), PortId(WEST))),
            WEST if c.x > 0 => Some((self.id(c.x - 1, c.y), PortId(EAST))),
            NORTH if c.y + 1 < self.height => Some((self.id(c.x, c.y + 1), PortId(SOUTH))),
            SOUTH if c.y > 0 => Some((self.id(c.x, c.y - 1), PortId(NORTH))),
            _ => None,
        }
    }

    fn port_for_label(&self, r: RouterId, label: PortLabel) -> Option<PortId> {
        let c = self.coord(r);
        match label {
            PortLabel::Local => Some(PortId(LOCAL)),
            PortLabel::XPlus if c.x + 1 < self.width => Some(PortId(EAST)),
            PortLabel::XMinus if c.x > 0 => Some(PortId(WEST)),
            PortLabel::YPlus if c.y + 1 < self.height => Some(PortId(NORTH)),
            PortLabel::YMinus if c.y > 0 => Some(PortId(SOUTH)),
            _ => None,
        }
    }

    fn label_for_port(&self, _r: RouterId, p: PortId) -> PortLabel {
        match p.0 {
            EAST => PortLabel::XPlus,
            WEST => PortLabel::XMinus,
            NORTH => PortLabel::YPlus,
            SOUTH => PortLabel::YMinus,
            _ => PortLabel::Local,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_id_roundtrip_and_neighbors() {
        let m = Mesh2D::new(4, 4);
        assert_eq!(m.num_routers(), 16);
        let r = m.router_at(Coord::new(2, 1, 0)).unwrap();
        assert_eq!(m.coord(r), Coord::new(2, 1, 0));
        // East peer is (3,1) reached via its West port.
        let (peer, port) = m.adjacent(r, PortId(EAST)).unwrap();
        assert_eq!(m.coord(peer), Coord::new(3, 1, 0));
        assert_eq!(port.0, WEST);
        // Boundary has no neighbor.
        let corner = m.router_at(Coord::new(0, 0, 0)).unwrap();
        assert!(m.adjacent(corner, PortId(WEST)).is_none());
        assert!(m.adjacent(corner, PortId(SOUTH)).is_none());
    }
}
