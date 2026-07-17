//! A 2D torus: [`Mesh2D`](crate::Mesh2D) with wraparound X and Y edges. The
//! wraparound link in each dimension is the *dateline*: dimension-order
//! routing must switch to a dedicated upper-VC-half class when it crosses one
//! to stay deadlock-free (P1-F12); see [`Topology::is_dateline`].

use tessera_core::{Coord, PortId, PortLabel, RouterId, Topology};

use crate::mesh2d::{EAST, LOCAL, NORTH, PORTS, SOUTH, WEST};

pub struct Torus2D {
    width: u16,
    height: u16,
}

impl Torus2D {
    pub fn new(width: u16, height: u16) -> Self {
        assert!(
            width >= 2 && height >= 2,
            "torus dimensions must be >= 2 (a size-1 ring would duplicate its own link)"
        );
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

impl Topology for Torus2D {
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
            EAST => Some((self.id((c.x + 1) % self.width, c.y), PortId(WEST))),
            WEST => Some((
                self.id((c.x + self.width - 1) % self.width, c.y),
                PortId(EAST),
            )),
            NORTH => Some((self.id(c.x, (c.y + 1) % self.height), PortId(SOUTH))),
            SOUTH => Some((
                self.id(c.x, (c.y + self.height - 1) % self.height),
                PortId(NORTH),
            )),
            _ => None,
        }
    }

    fn port_for_label(&self, _r: RouterId, label: PortLabel) -> Option<PortId> {
        // Every router has every direction on a torus (no boundary).
        match label {
            PortLabel::Local => Some(PortId(LOCAL)),
            PortLabel::XPlus => Some(PortId(EAST)),
            PortLabel::XMinus => Some(PortId(WEST)),
            PortLabel::YPlus => Some(PortId(NORTH)),
            PortLabel::YMinus => Some(PortId(SOUTH)),
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

    fn is_dateline(&self, r: RouterId, p: PortId) -> bool {
        let c = self.coord(r);
        match p.0 {
            EAST => c.x == self.width - 1,
            WEST => c.x == 0,
            NORTH => c.y == self.height - 1,
            SOUTH => c.y == 0,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_both_edges() {
        let t = Torus2D::new(4, 4);
        let corner = t.router_at(Coord::new(0, 0, 0)).unwrap();
        let (west_peer, port) = t.adjacent(corner, PortId(WEST)).unwrap();
        assert_eq!(
            t.coord(west_peer),
            Coord::new(3, 0, 0),
            "wraps west to east edge"
        );
        assert_eq!(port.0, EAST);

        let (south_peer, port) = t.adjacent(corner, PortId(SOUTH)).unwrap();
        assert_eq!(
            t.coord(south_peer),
            Coord::new(0, 3, 0),
            "wraps south to north edge"
        );
        assert_eq!(port.0, NORTH);

        // A mesh boundary has no such neighbor; a torus always does.
        let far_corner = t.router_at(Coord::new(3, 3, 0)).unwrap();
        assert!(t.adjacent(far_corner, PortId(EAST)).is_some());
        assert!(t.adjacent(far_corner, PortId(NORTH)).is_some());
    }

    #[test]
    fn dateline_marks_exactly_the_wraparound_edge() {
        let t = Torus2D::new(4, 4);
        let origin = t.router_at(Coord::new(0, 0, 0)).unwrap();
        assert!(t.is_dateline(origin, PortId(WEST)), "x=0 going west wraps");
        assert!(
            t.is_dateline(origin, PortId(SOUTH)),
            "y=0 going south wraps"
        );
        assert!(
            !t.is_dateline(origin, PortId(EAST)),
            "interior-ward, not a wrap"
        );
        assert!(
            !t.is_dateline(origin, PortId(NORTH)),
            "interior-ward, not a wrap"
        );

        let far = t.router_at(Coord::new(3, 3, 0)).unwrap();
        assert!(
            t.is_dateline(far, PortId(EAST)),
            "x=width-1 going east wraps"
        );
        assert!(
            t.is_dateline(far, PortId(NORTH)),
            "y=height-1 going north wraps"
        );
    }
}
