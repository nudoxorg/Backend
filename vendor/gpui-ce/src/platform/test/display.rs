use crate::{Bounds, DisplayId, Pixels, PlatformDisplay, Point, px};
use anyhow::{Ok, Result};

#[derive(Debug)]
pub(crate) struct TestDisplay {
    id: DisplayId,
    uuid: uuid::Uuid,
    bounds: Bounds<Pixels>,
}

impl TestDisplay {
    pub fn new() -> Self {
        TestDisplay {
            id: DisplayId(1),
            // NUDOX: the headless machine's one display is the same display
            // on every launch, as a person's monitor is (per-display settings,
            // such as the zoom, are keyed by it and must survive a relaunch).
            uuid: uuid::Uuid::from_u128(0x6e75_646f_7865_6865_6164_6c65_7373_0001),
            bounds: Bounds::from_corners(Point::default(), Point::new(px(1920.), px(1080.))),
        }
    }
}

impl PlatformDisplay for TestDisplay {
    fn id(&self) -> crate::DisplayId {
        self.id
    }

    fn uuid(&self) -> Result<uuid::Uuid> {
        Ok(self.uuid)
    }

    fn bounds(&self) -> crate::Bounds<crate::Pixels> {
        self.bounds
    }
}
