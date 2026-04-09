use std::cell::RefCell;
use std::rc::Rc;

/// Interpreter-internal owned reference. Single-threaded only.
/// In AOT mode this is lowered to Box<T> or alloca.
/// Never exposed to user code.
#[derive(Debug, Clone)]
pub struct OwnedRef<T>(pub Rc<RefCell<T>>);

impl<T> OwnedRef<T> {
    pub fn new(val: T) -> Self {
        OwnedRef(Rc::new(RefCell::new(val)))
    }

    pub fn identity(&self) -> usize {
        Rc::as_ptr(&self.0) as usize
    }

    pub fn borrow(&self) -> std::cell::Ref<'_, T> {
        self.0.borrow()
    }
    pub fn borrow_mut(&self) -> std::cell::RefMut<'_, T> {
        self.0.borrow_mut()
    }
    pub fn clone_ref(&self) -> Self {
        OwnedRef(Rc::clone(&self.0))
    }
}

impl<T: Clone> OwnedRef<T> {
    pub fn deep_clone(&self) -> Self {
        OwnedRef::new(self.0.borrow().clone())
    }
}
