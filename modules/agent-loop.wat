(component
  (import "event-count" (func $event-count (result s64)))
  (import "read-event" (func $read-event (param "index" u64) (result s64)))
  (import "resume-event" (func $resume-event (param "index" u64) (param "value" u64) (result s32)))
  (import "resume-exchange" (func $resume-exchange (param "index" u64) (param "value" u64) (result s32)))
  (import "call-provider" (func $call-provider (param "slot" u64) (param "operation" u64)
    (param "arg0" u64) (param "arg1" u64) (result s64)))
  (import "set-state" (func $set-state (param "key" u64) (param "value" u64) (result s32)))
  (import "publish" (func $publish (param "slot" u64) (param "value" u64) (result s32)))
  (core func $event-count-core (canon lower (func $event-count)))
  (core func $read-event-core (canon lower (func $read-event)))
  (core func $resume-event-core (canon lower (func $resume-event)))
  (core func $resume-exchange-core (canon lower (func $resume-exchange)))
  (core func $call-provider-core (canon lower (func $call-provider)))
  (core func $set-state-core (canon lower (func $set-state)))
  (core func $publish-core (canon lower (func $publish)))
  (core module $module
    (import "host" "event-count" (func $event-count (result i64)))
    (import "host" "read-event" (func $read-event (param i64) (result i64)))
    (import "host" "resume-event" (func $resume-event (param i64 i64) (result i32)))
    (import "host" "resume-exchange" (func $resume-exchange (param i64 i64) (result i32)))
    (import "host" "call-provider" (func $call-provider (param i64 i64 i64 i64) (result i64)))
    (import "host" "set-state" (func $set-state (param i64 i64) (result i32)))
    (import "host" "publish" (func $publish (param i64 i64) (result i32)))
    (func $fact (param $kind i64) (param $turn i64) (param $invocation i64)
      (param $data i64) (result i64)
      local.get $kind
      i64.const 56
      i64.shl
      local.get $turn
      i64.const 48
      i64.shl
      i64.or
      local.get $invocation
      i64.const 32
      i64.shl
      i64.or
      local.get $data
      i64.or)
    (func $commit (param $value i64) (result i32)
      (local $status i32)
      i64.const 0
      local.get $value
      call $resume-event
      local.set $status
      local.get $status
      i32.eqz
      if (result i32)
        i32.const 1
      else
        i32.const 0
        local.get $status
        i32.sub
      end)
    (func $commit-provider (param $value i64) (result i32)
      (local $status i32)
      i64.const 0
      local.get $value
      call $resume-exchange
      local.set $status
      local.get $status
      i32.const 3
      i32.eq
      if
        i64.const 0
        local.get $value
        call $resume-event
        local.set $status
      end
      local.get $status
      i32.eqz
      if (result i32)
        i32.const 1
      else
        i32.const 0
        local.get $status
        i32.sub
      end)
    (func (export "start") (param $config i64) (result i64)
      local.get $config
      i64.const 1
      i64.add)
    (func (export "step") (param $instance i64) (result i32)
      (local $count i64)
      (local $index i64)
      (local $value i64)
      (local $kind i64)
      (local $event-turn i64)
      (local $turn i64)
      (local $prompt i64)
      (local $request1 i32)
      (local $tool-call i32)
      (local $tool-kind i64)
      (local $tool-result-set i32)
      (local $tool-result i64)
      (local $request2 i32)
      (local $message-set i32)
      (local $message i64)
      (local $usage i32)
      (local $stop i32)
      (local $interrupted i32)
      (local $invocation i64)
      (local $response i64)
      call $event-count
      local.tee $count
      i64.const 0
      i64.lt_s
      if
        local.get $count
        i32.wrap_i64
        return
      end
      block $scanned
        loop $scan
          local.get $index
          local.get $count
          i64.ge_u
          br_if $scanned
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
          i64.const 127
          i64.and
          local.set $kind
          local.get $value
          i64.const 48
          i64.shr_u
          i64.const 255
          i64.and
          local.set $event-turn
          local.get $kind
          i64.const 1
          i64.eq
          if
            local.get $event-turn
            local.get $turn
            i64.gt_u
            if
              local.get $event-turn
              local.set $turn
              local.get $value
              i64.const 4294967295
              i64.and
              local.set $prompt
              i32.const 0
              local.set $request1
              i32.const 0
              local.set $tool-call
              i64.const 0
              local.set $tool-kind
              i32.const 0
              local.set $tool-result-set
              i64.const 0
              local.set $tool-result
              i32.const 0
              local.set $request2
              i32.const 0
              local.set $message-set
              i64.const 0
              local.set $message
              i32.const 0
              local.set $usage
              i32.const 0
              local.set $stop
              i32.const 0
              local.set $interrupted
            end
          else
            local.get $event-turn
            local.get $turn
            i64.eq
            local.get $turn
            i64.const 0
            i64.ne
            i32.and
            if
              local.get $kind
              i64.const 2
              i64.eq
              if
                local.get $value
                i64.const 4294967295
                i64.and
                i64.const 1
                i64.eq
                if
                  i32.const 1
                  local.set $request1
                else
                  i32.const 1
                  local.set $request2
                end
              end
              local.get $kind
              i64.const 3
              i64.eq
              if
                i32.const 1
                local.set $tool-call
                local.get $value
                i64.const 4294967295
                i64.and
                local.set $tool-kind
              end
              local.get $kind
              i64.const 4
              i64.eq
              if
                i32.const 1
                local.set $tool-result-set
                local.get $value
                i64.const 4294967295
                i64.and
                local.set $tool-result
              end
              local.get $kind
              i64.const 5
              i64.eq
              if
                i32.const 1
                local.set $message-set
                local.get $value
                i64.const 4294967295
                i64.and
                local.set $message
              end
              local.get $kind
              i64.const 6
              i64.eq
              if
                i32.const 1
                local.set $usage
              end
              local.get $kind
              i64.const 7
              i64.eq
              if
                i32.const 1
                local.set $stop
              end
              local.get $kind
              i64.const 8
              i64.eq
              if
                i32.const 1
                local.set $interrupted
              end
            end
          end
          local.get $index
          i64.const 1
          i64.add
          local.set $index
          br $scan
        end
      end
      i64.const 910
      local.get $turn
      call $set-state
      drop
      i64.const 911
      local.get $count
      call $set-state
      drop
      i64.const 912
      local.get $tool-result
      call $set-state
      drop
      i64.const 913
      local.get $message
      call $set-state
      drop
      i64.const 13
      local.get $message
      call $publish
      drop
      local.get $turn
      i64.eqz
      if
        i32.const 1
        return
      end
      local.get $stop
      if
        i32.const 1
        return
      end
      local.get $interrupted
      if
        i64.const 7
        local.get $turn
        i64.const 0
        i64.const 2
        call $fact
        call $commit
        return
      end
      local.get $request1
      i32.eqz
      if
        local.get $turn
        i64.const 16
        i64.mul
        i64.const 1
        i64.add
        local.set $invocation
        i64.const 2
        local.get $turn
        local.get $invocation
        i64.const 1
        call $fact
        call $commit
        return
      end
      local.get $instance
      i64.const 2
      i64.eq
      if
        local.get $message-set
        if
          local.get $usage
          i32.eqz
          if
            i64.const 6
            local.get $turn
            i64.const 0
            local.get $message
            call $fact
            call $commit
            return
          else
            i64.const 7
            local.get $turn
            i64.const 0
            i64.const 1
            call $fact
            call $commit
            return
          end
        end
      end
      local.get $tool-call
      i32.eqz
      if
        local.get $turn
        i64.const 16
        i64.mul
        i64.const 1
        i64.add
        local.set $invocation
        i64.const 10
        i64.const 1
        local.get $invocation
        local.get $prompt
        call $call-provider
        local.tee $response
        i64.const 0
        i64.lt_s
        if
          local.get $response
          i64.const -26
          i64.ge_s
          local.get $response
          i64.const -10
          i64.le_s
          i32.and
          if
            i64.const 8
            local.get $turn
            local.get $invocation
            i64.const 1
            call $fact
            call $commit
            return
          end
          local.get $response
          i32.wrap_i64
          return
        end
        local.get $turn
        i64.const 16
        i64.mul
        i64.const 2
        i64.add
        local.set $invocation
        local.get $instance
        i64.const 2
        i64.eq
        if (result i64)
          i64.const 5
        else
          i64.const 3
        end
        local.get $turn
        local.get $invocation
        local.get $response
        call $fact
        call $commit-provider
        return
      end
      local.get $tool-kind
      i64.const 2
      i64.eq
      if
        local.get $turn
        i64.const 16
        i64.mul
        i64.const 2
        i64.add
        local.set $invocation
        i64.const 8
        local.get $turn
        local.get $invocation
        i64.const 1
        call $fact
        call $commit
        return
      end
      local.get $tool-result-set
      i32.eqz
      if
        local.get $turn
        i64.const 16
        i64.mul
        i64.const 2
        i64.add
        local.set $invocation
        i64.const 11
        i64.const 1
        local.get $invocation
        local.get $prompt
        call $call-provider
        local.tee $response
        i64.const 0
        i64.lt_s
        if
          local.get $response
          i64.const -26
          i64.ge_s
          local.get $response
          i64.const -10
          i64.le_s
          i32.and
          if
            i64.const 8
            local.get $turn
            local.get $invocation
            i64.const 1
            call $fact
            call $commit
            return
          end
          local.get $response
          i32.wrap_i64
          return
        end
        i64.const 4
        local.get $turn
        local.get $invocation
        local.get $response
        call $fact
        call $commit-provider
        return
      end
      local.get $request2
      i32.eqz
      if
        local.get $turn
        i64.const 16
        i64.mul
        i64.const 3
        i64.add
        local.set $invocation
        i64.const 2
        local.get $turn
        local.get $invocation
        i64.const 2
        call $fact
        call $commit
        return
      end
      local.get $message-set
      i32.eqz
      if
        local.get $turn
        i64.const 16
        i64.mul
        i64.const 3
        i64.add
        local.set $invocation
        i64.const 10
        i64.const 2
        local.get $invocation
        local.get $tool-result
        call $call-provider
        local.tee $response
        i64.const 0
        i64.lt_s
        if
          local.get $response
          i64.const -26
          i64.ge_s
          local.get $response
          i64.const -10
          i64.le_s
          i32.and
          if
            i64.const 8
            local.get $turn
            local.get $invocation
            i64.const 1
            call $fact
            call $commit
            return
          end
          local.get $response
          i32.wrap_i64
          return
        end
        i64.const 5
        local.get $turn
        local.get $invocation
        local.get $response
        call $fact
        call $commit-provider
        return
      end
      local.get $usage
      i32.eqz
      if
        local.get $turn
        i64.const 16
        i64.mul
        i64.const 3
        i64.add
        local.set $invocation
        i64.const 6
        local.get $turn
        local.get $invocation
        i64.const 7
        call $fact
        call $commit
        return
      end
      i64.const 7
      local.get $turn
      i64.const 0
      i64.const 1
      call $fact
      call $commit)
    (func (export "invoke") (param i64 i64 i64 i64) (result i64) i64.const -4)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "event-count" (func $event-count-core))
    (export "read-event" (func $read-event-core))
    (export "resume-event" (func $resume-event-core))
    (export "resume-exchange" (func $resume-exchange-core))
    (export "call-provider" (func $call-provider-core))
    (export "set-state" (func $set-state-core))
    (export "publish" (func $publish-core)))
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
