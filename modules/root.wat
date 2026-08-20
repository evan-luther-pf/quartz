(component
  (import "register-child" (func $register-child (param "index" u32) (result s32)))
  (core func $register-child-core (canon lower (func $register-child)))
  (core module $module
    (import "host" "register-child" (func $register-child (param i32) (result i32)))
    (global $count (mut i64) (i64.const 0))
    (global $index (mut i64) (i64.const 0))
    (func (export "start") (param $config i64) (result i64)
      local.get $config
      global.set $count
      i64.const 0
      global.set $index
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $status i32)
      global.get $index
      global.get $count
      i64.ge_u
      if
        i32.const 1
        return
      end
      global.get $index
      i32.wrap_i64
      call $register-child
      local.tee $status
      i32.const 0
      i32.ne
      if
        i32.const 0
        local.get $status
        i32.sub
        return
      end
      global.get $index
      i64.const 1
      i64.add
      global.set $index
      global.get $index
      global.get $count
      i64.eq
      if (result i32)
        i32.const 1
      else
        i32.const 0
      end)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "register-child" (func $register-child-core)))
  (core instance $instance (instantiate $module (with "host" (instance $host))))
  (func (export "start") (param "config" u64) (result u64)
    (canon lift (core func $instance "start")))
  (func (export "step") (param "instance" u64) (result s32)
    (canon lift (core func $instance "step")))
  (func (export "drop") (param "instance" u64)
    (canon lift (core func $instance "drop"))))
