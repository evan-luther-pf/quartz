(component
  (import "publish" (func $publish (param "slot" u64) (param "value" u64) (result s32)))
  (core func $publish-core (canon lower (func $publish)))
  (core module $module
    (import "host" "publish" (func $publish (param i64 i64) (result i32)))
    (func (export "start") (param i64) (result i64) i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $status i32)
      i64.const 1
      i64.const 2
      call $publish
      local.tee $status
      i32.eqz
      if (result i32)
        i32.const 1
      else
        i32.const 0
        local.get $status
        i32.sub
      end)
    (func (export "drop") (param i64)))
  (core instance $host (export "publish" (func $publish-core)))
  (core instance $instance (instantiate $module (with "host" (instance $host))))
  (func (export "start") (param "config" u64) (result u64)
    (canon lift (core func $instance "start")))
  (func (export "step") (param "instance" u64) (result s32)
    (canon lift (core func $instance "step")))
  (func (export "drop") (param "instance" u64)
    (canon lift (core func $instance "drop"))))
