pub mod context;
pub mod handle;

use std::any::Any;

pub use self::context::*;
pub use self::handle::*;
use crate::Entity;

pub trait AnyModel {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn type_name(&self) -> &'static str;
}

impl<T> AnyModel for T
where
    T: Entity,
{
    fn type_name(&self) -> &'static str {
        std::any::type_name::<T>()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
