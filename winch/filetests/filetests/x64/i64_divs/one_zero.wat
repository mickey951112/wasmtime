;;! target = "x86_64"

(module
    (func (result i64)
	(i64.const 1)
	(i64.const 0)
	(i64.div_s)
    )
)
;;    0:	 55                   	push	rbp
;;    1:	 4889e5               	mov	rbp, rsp
;;    4:	 48c7c100000000       	mov	rcx, 0
;;    b:	 48c7c001000000       	mov	rax, 1
;;   12:	 4883f900             	cmp	rcx, 0
;;   16:	 0f8502000000         	jne	0x1e
;;   1c:	 0f0b                 	ud2	
;;   1e:	 4899                 	cqo	
;;   20:	 48f7f9               	idiv	rcx
;;   23:	 5d                   	pop	rbp
;;   24:	 c3                   	ret	
