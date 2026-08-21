(component
  (import "publish-callable" (func $publish-callable (param "slot" u64) (result s32)))
  (core func $publish-callable-core (canon lower (func $publish-callable)))
  (core module $module
    (import "host" "publish-callable" (func $publish-callable (param i64) (result i32)))
    (func (export "start") (param i64) (result i64) i64.const 1)
    (func (export "step") (param i64) (result i32)
      i64.const 12
      call $publish-callable
      i32.eqz
      if (result i32)
        i32.const 1
      else
        i32.const -3
      end)
    (func (export "invoke") (param i64) (param $operation i64)
      (param $prompt i64) (param i64) (result i64)
      local.get $operation
      i64.const 1
      i64.ne
      if
        i64.const -3
        return
      end
      local.get $prompt
      i64.const 1
      i64.lt_u
      local.get $prompt
      i64.const 3
      i64.gt_u
      i32.or
      if (result i64)
        i64.const -3
      else
        local.get $prompt
      end)
    (func (export "drop") (param i64)))
  (core instance $host (export "publish-callable" (func $publish-callable-core)))
  (core instance $instance (instantiate $module (with "host" (instance $host))))
  (func (export "start") (param "config" u64) (result u64)
    (canon lift (core func $instance "start")))
  (func (export "step") (param "instance" u64) (result s32)
    (canon lift (core func $instance "step")))
  (func (export "invoke") (param "instance" u64) (param "operation" u64)
    (param "arg0" u64) (param "arg1" u64) (result s64)
    (canon lift (core func $instance "invoke")))
  (func (export "drop") (param "instance" u64)
    (canon lift (core func $instance "drop"))))
