use crate::error::GuestError;
use crate::region::Region;
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct BorrowHandle(usize);

pub struct BorrowChecker {
    bc: RefCell<InnerBorrowChecker>,
}

impl BorrowChecker {
    pub fn new() -> Self {
        BorrowChecker {
            bc: RefCell::new(InnerBorrowChecker::new()),
        }
    }
    pub fn borrow(&self, r: Region) -> Result<BorrowHandle, GuestError> {
        self.bc
            .borrow_mut()
            .borrow(r)
            .ok_or_else(|| GuestError::PtrBorrowed(r))
    }
    pub fn unborrow(&self, h: BorrowHandle) {
        self.bc.borrow_mut().unborrow(h)
    }
}

#[derive(Debug)]
struct InnerBorrowChecker {
    borrows: HashMap<BorrowHandle, Region>,
    next_handle: BorrowHandle,
}

impl InnerBorrowChecker {
    fn new() -> Self {
        InnerBorrowChecker {
            borrows: HashMap::new(),
            next_handle: BorrowHandle(0),
        }
    }

    fn is_borrowed(&self, r: Region) -> bool {
        !self.borrows.values().all(|b| !b.overlaps(r))
    }

    fn new_handle(&mut self) -> BorrowHandle {
        let h = self.next_handle;
        self.next_handle = BorrowHandle(h.0 + 1);
        h
    }

    fn borrow(&mut self, r: Region) -> Option<BorrowHandle> {
        if self.is_borrowed(r) {
            return None;
        }
        let h = self.new_handle();
        self.borrows.insert(h, r);
        Some(h)
    }

    fn unborrow(&mut self, h: BorrowHandle) {
        let _ = self.borrows.remove(&h);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn nonoverlapping() {
        let mut bs = InnerBorrowChecker::new();
        let r1 = Region::new(0, 10);
        let r2 = Region::new(10, 10);
        assert!(!r1.overlaps(r2));
        bs.borrow(r1).expect("can borrow r1");
        bs.borrow(r2).expect("can borrow r2");

        let mut bs = InnerBorrowChecker::new();
        let r1 = Region::new(10, 10);
        let r2 = Region::new(0, 10);
        assert!(!r1.overlaps(r2));
        bs.borrow(r1).expect("can borrow r1");
        bs.borrow(r2).expect("can borrow r2");
    }

    #[test]
    fn overlapping() {
        let mut bs = InnerBorrowChecker::new();
        let r1 = Region::new(0, 10);
        let r2 = Region::new(9, 10);
        assert!(r1.overlaps(r2));
        bs.borrow(r1).expect("can borrow r1");
        assert!(bs.borrow(r2).is_none(), "cant borrow r2");

        let mut bs = InnerBorrowChecker::new();
        let r1 = Region::new(0, 10);
        let r2 = Region::new(2, 5);
        assert!(r1.overlaps(r2));
        bs.borrow(r1).expect("can borrow r1");
        assert!(bs.borrow(r2).is_none(), "cant borrow r2");

        let mut bs = InnerBorrowChecker::new();
        let r1 = Region::new(9, 10);
        let r2 = Region::new(0, 10);
        assert!(r1.overlaps(r2));
        bs.borrow(r1).expect("can borrow r1");
        assert!(bs.borrow(r2).is_none(), "cant borrow r2");

        let mut bs = InnerBorrowChecker::new();
        let r1 = Region::new(2, 5);
        let r2 = Region::new(0, 10);
        assert!(r1.overlaps(r2));
        bs.borrow(r1).expect("can borrow r1");
        assert!(bs.borrow(r2).is_none(), "cant borrow r2");

        let mut bs = InnerBorrowChecker::new();
        let r1 = Region::new(2, 5);
        let r2 = Region::new(10, 5);
        let r3 = Region::new(15, 5);
        let r4 = Region::new(0, 10);
        assert!(r1.overlaps(r4));
        bs.borrow(r1).expect("can borrow r1");
        bs.borrow(r2).expect("can borrow r2");
        bs.borrow(r3).expect("can borrow r3");
        assert!(bs.borrow(r4).is_none(), "cant borrow r4");
    }

    #[test]
    fn unborrowing() {
        let mut bs = InnerBorrowChecker::new();
        let r1 = Region::new(0, 10);
        let r2 = Region::new(10, 10);
        assert!(!r1.overlaps(r2));
        let _h1 = bs.borrow(r1).expect("can borrow r1");
        let h2 = bs.borrow(r2).expect("can borrow r2");

        assert!(bs.borrow(r2).is_none(), "can't borrow r2 twice");
        bs.unborrow(h2);

        let _h3 = bs
            .borrow(r2)
            .expect("can borrow r2 again now that its been unborrowed");
    }
}
