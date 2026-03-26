//! Animation state machine for the tray icon spin effect.
//!
//! When Omnibus is starting up or processing, the pinwheel spins.
//! When idle, it stays static. The controller tracks which frame to
//! display and advances on each tick.

/// Current animation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationState {
    /// Icon is static (idle).
    Static,
    /// Icon is spinning (active/connecting).
    Spinning,
}

/// Controls the tray icon spin animation.
///
/// Call [`tick`](AnimationController::tick) at regular intervals (~80ms).
/// When spinning, it returns the next frame index. When static, it returns `None`.
pub struct AnimationController {
    state: AnimationState,
    current_frame: usize,
    frame_count: usize,
}

impl AnimationController {
    /// Create a new controller with the given number of animation frames.
    pub fn new(frame_count: usize) -> Self {
        Self {
            state: AnimationState::Static,
            current_frame: 0,
            frame_count,
        }
    }

    /// Start the spin animation from the current frame.
    pub fn start_spinning(&mut self) {
        self.state = AnimationState::Spinning;
    }

    /// Stop the spin animation. The icon returns to the static frame.
    pub fn stop_spinning(&mut self) {
        self.state = AnimationState::Static;
        self.current_frame = 0;
    }

    /// Advance the animation by one frame.
    ///
    /// Returns `Some(frame_index)` if the animation is spinning and the frame
    /// changed, or `None` if the animation is static.
    pub fn tick(&mut self) -> Option<usize> {
        match self.state {
            AnimationState::Static => None,
            AnimationState::Spinning => {
                self.current_frame = (self.current_frame + 1) % self.frame_count;
                Some(self.current_frame)
            }
        }
    }

    /// Whether the icon is currently spinning.
    pub fn is_spinning(&self) -> bool {
        self.state == AnimationState::Spinning
    }

    /// The current animation state.
    #[allow(dead_code)] // used in tests + available for future consumers
    pub fn state(&self) -> AnimationState {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_is_static() {
        let ctrl = AnimationController::new(12);
        assert_eq!(ctrl.state(), AnimationState::Static);
        assert!(!ctrl.is_spinning());
    }

    #[test]
    fn test_tick_while_static_returns_none() {
        let mut ctrl = AnimationController::new(12);
        assert_eq!(ctrl.tick(), None);
        assert_eq!(ctrl.tick(), None);
    }

    #[test]
    fn test_spinning_tick_advances_frames() {
        let mut ctrl = AnimationController::new(4);
        ctrl.start_spinning();
        assert!(ctrl.is_spinning());

        assert_eq!(ctrl.tick(), Some(1));
        assert_eq!(ctrl.tick(), Some(2));
        assert_eq!(ctrl.tick(), Some(3));
        // Wraps around
        assert_eq!(ctrl.tick(), Some(0));
        assert_eq!(ctrl.tick(), Some(1));
    }

    #[test]
    fn test_stop_resets_to_zero() {
        let mut ctrl = AnimationController::new(12);
        ctrl.start_spinning();
        ctrl.tick(); // frame 1
        ctrl.tick(); // frame 2
        ctrl.stop_spinning();

        assert_eq!(ctrl.state(), AnimationState::Static);
        assert_eq!(ctrl.tick(), None);
    }

    #[test]
    fn test_start_stop_start_continues() {
        let mut ctrl = AnimationController::new(4);
        ctrl.start_spinning();
        ctrl.tick(); // 1
        ctrl.tick(); // 2
        ctrl.stop_spinning();
        ctrl.start_spinning();
        // Resets to 0, first tick goes to 1
        assert_eq!(ctrl.tick(), Some(1));
    }
}
