;;! target = "x86_64"

(module
    (export "main" (func $main))

    (func $main (result i32)
	(i32.const 10)
	(i32.const 20)
	i32.add)
)
;;    0:	 55                   	push	rbp
;;    1:	 4889e5               	mov	rbp, rsp
;;    4:	 b80a000000           	mov	eax, 0xa
;;    9:	 83c014               	add	eax, 0x14
;;    c:	 5d                   	pop	rbp
;;    d:	 c3                   	ret	
