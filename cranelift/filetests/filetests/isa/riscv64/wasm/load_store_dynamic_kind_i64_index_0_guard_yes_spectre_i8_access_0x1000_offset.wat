;;! target = "riscv64"
;;!
;;! settings = ['enable_heap_access_spectre_mitigation=true']
;;!
;;! compile = true
;;!
;;! [globals.vmctx]
;;! type = "i64"
;;! vmctx = true
;;!
;;! [globals.heap_base]
;;! type = "i64"
;;! load = { base = "vmctx", offset = 0, readonly = true }
;;!
;;! [globals.heap_bound]
;;! type = "i64"
;;! load = { base = "vmctx", offset = 8, readonly = true }
;;!
;;! [[heaps]]
;;! base = "heap_base"
;;! min_size = 0x10000
;;! offset_guard_size = 0
;;! index_type = "i64"
;;! style = { kind = "dynamic", bound = "heap_bound" }

;; !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
;; !!! GENERATED BY 'make-load-store-tests.sh' DO NOT EDIT !!!
;; !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!

(module
  (memory i64 1)

  (func (export "do_store") (param i64 i32)
    local.get 0
    local.get 1
    i32.store8 offset=0x1000)

  (func (export "do_load") (param i64) (result i32)
    local.get 0
    i32.load8_u offset=0x1000))

;; function u0:0:
;; block0:
;;   ld a4,8(a2)
;;   lui a5,1048575
;;   addi a5,a5,4095
;;   add a4,a4,a5
;;   ugt a5,a0,a4##ty=i64
;;   ld a4,0(a2)
;;   add a4,a4,a0
;;   lui a6,1
;;   add a4,a4,a6
;;   li a6,0
;;   andi t1,a5,255
;;   sltu a0,zero,t1
;;   sub a2,zero,a0
;;   and a5,a6,a2
;;   not a6,a2
;;   and t3,a4,a6
;;   or t0,a5,t3
;;   sb a1,0(t0)
;;   j label1
;; block1:
;;   ret
;;
;; function u0:1:
;; block0:
;;   ld a4,8(a1)
;;   lui a5,1048575
;;   addi a5,a5,4095
;;   add a4,a4,a5
;;   ugt a5,a0,a4##ty=i64
;;   ld a4,0(a1)
;;   add a4,a4,a0
;;   lui a6,1
;;   add a4,a4,a6
;;   li a6,0
;;   andi t1,a5,255
;;   sltu a0,zero,t1
;;   sub a2,zero,a0
;;   and a5,a6,a2
;;   not a6,a2
;;   and t3,a4,a6
;;   or t0,a5,t3
;;   lbu a0,0(t0)
;;   j label1
;; block1:
;;   ret
