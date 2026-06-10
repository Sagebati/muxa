//! HList-based application state.
//!
//! Plugins compose into a heterogeneous list of resources. Each `with_plugin`
//! call grows the list by one entry. Capability traits (in [`crate::capability`])
//! are implemented for any HList that contains the matching type, using the
//! [`Selector`] trait with phantom-typed indices ([`Here`] / [`There`]).

use core::marker::PhantomData;

/// Empty HList tail.
#[derive(Debug, Default, Clone, Copy)]
pub struct HNil;

/// Non-empty HList: `head` value of type `H`, then the rest in `tail`.
#[derive(Debug, Clone, Copy)]
pub struct HCons<H, T> {
    /// The head value.
    pub head: H,
    /// The remainder of the list.
    pub tail: T,
}

/// Phantom index: "the value is at this position".
pub struct Here;

/// Phantom index: "the value is one step further in".
pub struct There<I>(PhantomData<I>);

/// Selector trait: any HList containing a `T` somewhere has `Selector<T, _>`
/// for some phantom index. Used as a blanket-implementable supertrait by
/// capability traits like [`crate::capability::HasPgExecutorFor`].
pub trait Selector<T, I> {
    /// Borrow the contained value.
    fn select(&self) -> &T;
}

impl<T, Tail> Selector<T, Here> for HCons<T, Tail> {
    fn select(&self) -> &T {
        &self.head
    }
}

impl<Head, Tail, T, I> Selector<T, There<I>> for HCons<Head, Tail>
where
    Tail: Selector<T, I>,
{
    fn select(&self) -> &T {
        self.tail.select()
    }
}

/// Marker trait for a "state HList" — convenience supertrait that bounds
/// `Send + Sync + 'static` and exposes the [`State::Push`] GAT for chaining.
pub trait State: Send + Sync + 'static + Sized {
    /// The new state type after pushing a `T`.
    type Push<T: Send + Sync + 'static>: State;

    /// Append `value` to the head of the list, growing the type.
    fn push<T: Send + Sync + 'static>(self, value: T) -> Self::Push<T>;
}

impl State for HNil {
    type Push<T: Send + Sync + 'static> = HCons<T, HNil>;

    fn push<T: Send + Sync + 'static>(self, value: T) -> HCons<T, HNil> {
        HCons {
            head: value,
            tail: HNil,
        }
    }
}

impl<H, Tail> State for HCons<H, Tail>
where
    H: Send + Sync + 'static,
    Tail: State,
{
    type Push<T: Send + Sync + 'static> = HCons<T, HCons<H, Tail>>;

    fn push<T: Send + Sync + 'static>(self, value: T) -> HCons<T, HCons<H, Tail>> {
        HCons {
            head: value,
            tail: self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_from_single() {
        let state = HNil.push(42u32);
        let value: &u32 = Selector::<u32, _>::select(&state);
        assert_eq!(*value, 42);
    }

    #[test]
    fn select_from_two() {
        let state = HNil.push(42u32).push("hi");
        let n: &u32 = Selector::<u32, _>::select(&state);
        let s_ref: &&str = Selector::<&str, _>::select(&state);
        assert_eq!(*n, 42);
        assert_eq!(*s_ref, "hi");
    }

    #[test]
    fn select_deeper() {
        // push order is head-first, so last pushed is at Here.
        let state = HNil.push(1u8).push(2u16).push(3u32).push(4u64);
        let first: &u8 = Selector::<u8, _>::select(&state);
        let second: &u16 = Selector::<u16, _>::select(&state);
        let third: &u32 = Selector::<u32, _>::select(&state);
        let fourth: &u64 = Selector::<u64, _>::select(&state);
        assert_eq!((*first, *second, *third, *fourth), (1, 2, 3, 4));
    }
}
