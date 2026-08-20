(component
  (import "resolve" (func $resolve (param "slot" u64) (result s64)))
  (core func $resolve-core (canon lower (func $resolve)))
  (core module $module
    (import "host" "resolve" (func $resolve (param i64) (result i64)))
    (func (export "start") (param i64) (result i64) i64.const 1)
    (func (export "step") (param i64) (result i32)
      i64.const 1
      call $resolve
      i32.wrap_i64)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "resolve" (func $resolve-core)))
  (core instance $instance (instantiate $module (with "host" (instance $host))))
  (func (export "start") (param "config" u64) (result u64)
    (canon lift (core func $instance "start")))
  (func (export "step") (param "instance" u64) (result s32)
    (canon lift (core func $instance "step")))
  (func (export "drop") (param "instance" u64)
    (canon lift (core func $instance "drop"))))
