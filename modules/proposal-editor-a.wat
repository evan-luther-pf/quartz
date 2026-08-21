(component
  (import "event-count" (func $event-count (result s64)))
  (import "read-event" (func $read-event (param "index" u64) (result s64)))
  (import "event-payload-len" (func $event-payload-len (param "index" u64) (result s64)))
  (import "event-payload-byte" (func $event-payload-byte
    (param "index" u64) (param "offset" u64) (result s32)))
  (import "workspace-set-len" (func $workspace-set-len
    (param "index" u64) (param "length" u64) (result s32)))
  (import "workspace-write-byte" (func $workspace-write-byte
    (param "index" u64) (param "offset" u64) (param "value" u32) (result s32)))
  (import "call-provider" (func $call-provider
    (param "slot" u64) (param "operation" u64) (param "arg0" u64) (param "arg1" u64)
    (result s64)))
  (import "publish-workspace" (func $publish-workspace (param "index" u64) (result s32)))
  (core func $event-count-core (canon lower (func $event-count)))
  (core func $read-event-core (canon lower (func $read-event)))
  (core func $event-payload-len-core (canon lower (func $event-payload-len)))
  (core func $event-payload-byte-core (canon lower (func $event-payload-byte)))
  (core func $workspace-set-len-core (canon lower (func $workspace-set-len)))
  (core func $workspace-write-byte-core (canon lower (func $workspace-write-byte)))
  (core func $call-provider-core (canon lower (func $call-provider)))
  (core func $publish-workspace-core (canon lower (func $publish-workspace)))
  (core module $module
    (import "host" "event-count" (func $event-count (result i64)))
    (import "host" "read-event" (func $read-event (param i64) (result i64)))
    (import "host" "event-payload-len" (func $event-payload-len (param i64) (result i64)))
    (import "host" "event-payload-byte" (func $event-payload-byte (param i64 i64) (result i32)))
    (import "host" "workspace-set-len" (func $workspace-set-len (param i64 i64) (result i32)))
    (import "host" "workspace-write-byte" (func $workspace-write-byte (param i64 i64 i32) (result i32)))
    (import "host" "call-provider" (func $call-provider (param i64 i64 i64 i64) (result i64)))
    (import "host" "publish-workspace" (func $publish-workspace (param i64) (result i32)))
    (global $turn (mut i64) (i64.const 0))
    (global $candidate (mut i64) (i64.const -1))
    (global $length (mut i64) (i64.const 0))
    (global $offset (mut i64) (i64.const 0))
    (global $phase (mut i32) (i32.const 0))
    (func (export "start") (param $config i64) (result i64)
      local.get $config
      global.set $turn
      i64.const -1
      global.set $candidate
      i64.const 0
      global.set $length
      i64.const 0
      global.set $offset
      i32.const 0
      global.set $phase
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $count i64)
      (local $index i64)
      (local $value i64)
      (local $payload-byte i32)
      (local $status i32)
      (local $copied i64)
      global.get $phase
      i32.eqz
      if
        call $event-count
        local.tee $count
        i64.const 0
        i64.lt_s
        if
          local.get $count
          i32.wrap_i64
          return
        end
        i64.const 0
        local.set $index
        block $scan-done
          loop $scan
            local.get $index
            local.get $count
            i64.ge_u
            br_if $scan-done
            local.get $index
            call $read-event
            local.tee $value
            i64.const 0
            i64.lt_s
            if
              local.get $value
              i32.wrap_i64
              return
            end
            local.get $value
            i64.const 56
            i64.shr_u
            i64.const 255
            i64.and
            i64.const 5
            i64.eq
            local.get $value
            i64.const 48
            i64.shr_u
            i64.const 255
            i64.and
            global.get $turn
            i64.eq
            i32.and
            if
              global.get $candidate
              i64.const -1
              i64.ne
              if
                i32.const -6
                return
              end
              local.get $index
              global.set $candidate
            end
            local.get $index
            i64.const 1
            i64.add
            local.set $index
            br $scan
          end
        end
        global.get $candidate
        i64.const -1
        i64.eq
        if
          i32.const -3
          return
        end
        i32.const 1
        global.set $phase
        i32.const 0
        return
      end
      global.get $phase
      i32.const 1
      i32.eq
      if
        global.get $candidate
        call $event-payload-len
        local.tee $value
        i64.const 0
        i64.lt_s
        if
          local.get $value
          i32.wrap_i64
          return
        end
        local.get $value
        global.set $length
        i64.const 0
        local.get $value
        call $workspace-set-len
        local.tee $status
        i32.eqz
        i32.eqz
        if
          i32.const 0
          local.get $status
          i32.sub
          return
        end
        i32.const 2
        global.set $phase
        i32.const 0
        return
      end
      global.get $phase
      i32.const 2
      i32.eq
      if
        i64.const 0
        local.set $copied
        block $copy-done
          loop $copy
            global.get $offset
            global.get $length
            i64.ge_u
            br_if $copy-done
            local.get $copied
            i64.const 4096
            i64.ge_u
            br_if $copy-done
            global.get $candidate
            global.get $offset
            call $event-payload-byte
            local.tee $payload-byte
            i32.const 0
            i32.lt_s
            if
              local.get $payload-byte
              return
            end
            i64.const 0
            global.get $offset
            local.get $payload-byte
            call $workspace-write-byte
            local.tee $status
            i32.eqz
            i32.eqz
            if
              i32.const 0
              local.get $status
              i32.sub
              return
            end
            global.get $offset
            i64.const 1
            i64.add
            global.set $offset
            local.get $copied
            i64.const 1
            i64.add
            local.set $copied
            br $copy
          end
        end
        global.get $offset
        global.get $length
        i64.lt_u
        if
          i32.const 0
          return
        end
        i32.const 3
        global.set $phase
        i32.const 0
        return
      end
      i64.const 12
      i64.const 1
      i64.const 8001
      i64.const 0
      call $call-provider
      i64.const 1
      i64.ne
      if
        i32.const -7
        return
      end
      i64.const 0
      call $publish-workspace
      local.tee $status
      i32.eqz
      if (result i32)
        i32.const 1
      else
        i32.const 0
        local.get $status
        i32.sub
      end)
    (func (export "invoke") (param i64 i64 i64 i64) (result i64) i64.const -4)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "event-count" (func $event-count-core))
    (export "read-event" (func $read-event-core))
    (export "event-payload-len" (func $event-payload-len-core))
    (export "event-payload-byte" (func $event-payload-byte-core))
    (export "workspace-set-len" (func $workspace-set-len-core))
    (export "workspace-write-byte" (func $workspace-write-byte-core))
    (export "call-provider" (func $call-provider-core))
    (export "publish-workspace" (func $publish-workspace-core)))
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
