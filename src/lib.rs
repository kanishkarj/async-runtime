#[macro_use]
extern crate lazy_static;

pub mod executor;
pub mod futures;
pub mod reactor;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
