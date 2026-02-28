use fidan_lexer::Symbol;
use fidan_runtime::FidanValue;
use std::collections::HashMap;

/// The evaluation environment: a stack of function frames.
///
/// Frame 0 is the module-level global scope (always present).
/// Each function call pushes a new frame that cannot see the
/// caller's locals, but *can* see the global frame (frame 0).
pub struct Env {
    /// Outer Vec = call frames (index 0 = global).
    /// Inner HashMap = local variables in that frame.
    frames: Vec<HashMap<Symbol, FidanValue>>,

    /// `this` binding — one slot per call frame.
    this_stack: Vec<Option<FidanValue>>,

    /// Per-frame name for the call stack.
    /// `None` for anonymous scopes (catch blocks, the global frame, etc.).
    /// `Some(name)` for named function / method invocations.
    frame_names: Vec<Option<String>>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            frames: vec![HashMap::new()],
            this_stack: vec![None],
            frame_names: vec![None], // global frame is anonymous
        }
    }

    // ── Frame management ───────────────────────────────────────────────────────

    /// Push a new call frame.
    ///
    /// * `name`  — the function / method name to appear in the call stack.
    ///   Pass `None` for anonymous scopes (catch blocks, closures, etc.).
    /// * `this`  — optional receiver object.
    pub fn push_frame(&mut self, name: Option<String>, this: Option<FidanValue>) {
        self.frames.push(HashMap::new());
        self.this_stack.push(this);
        self.frame_names.push(name);
    }

    /// Pop the top call frame.
    pub fn pop_frame(&mut self) {
        if self.frames.len() > 1 {
            self.frames.pop();
            self.this_stack.pop();
            self.frame_names.pop();
        }
    }

    /// Return the call stack as a list of named frames, **innermost first**.
    ///
    /// Anonymous frames (catch blocks, the global frame) are excluded.
    pub fn stack_trace(&self) -> Vec<String> {
        self.frame_names
            .iter()
            .rev()
            .filter_map(|n| n.clone())
            .collect()
    }

    // ── Variable access ────────────────────────────────────────────────────────

    /// Define a new variable in the current (innermost) frame.
    pub fn define(&mut self, name: Symbol, val: FidanValue) {
        if let Some(frame) = self.frames.last_mut() {
            frame.insert(name, val);
        }
    }

    /// Look up a variable: current frame first, then global frame.
    pub fn get(&self, name: Symbol) -> Option<&FidanValue> {
        let top = self.frames.last()?;
        if let Some(v) = top.get(&name) {
            return Some(v);
        }
        // Fall through to global scope (frame 0) if we're not already there.
        if self.frames.len() > 1 {
            return self.frames[0].get(&name);
        }
        None
    }

    /// Assign an *existing* variable — current frame first, then global frame.
    /// Returns `true` if the variable was found and updated.
    pub fn assign(&mut self, name: Symbol, val: FidanValue) -> bool {
        let n = self.frames.len();

        // Current frame
        if let Some(slot) = self.frames[n - 1].get(&name) {
            let _ = slot;
            self.frames[n - 1].insert(name, val);
            return true;
        }

        // Global frame (only when we're in a function, not already at global)
        if n > 1 {
            if self.frames[0].contains_key(&name) {
                self.frames[0].insert(name, val);
                return true;
            }
        }

        false
    }

    // ── `this` binding ─────────────────────────────────────────────────────────

    /// Get the current receiver (`this`), if any.
    pub fn this_val(&self) -> Option<&FidanValue> {
        self.this_stack.last()?.as_ref()
    }

    /// Replace the current `this` value (used internally).
    pub fn set_this(&mut self, val: FidanValue) {
        if let Some(slot) = self.this_stack.last_mut() {
            *slot = Some(val);
        }
    }
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}
