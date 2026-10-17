#![allow(dead_code)]
#[path="../../src/security/x86_access.rs"] mod access;
use access::*;
fn main() {
    assert_eq!(common([].into_iter()),0);
    assert_eq!(common([ALL,ALL].into_iter()),ALL);
    assert_eq!(common([ALL,SMEP|SMAP|UMIP].into_iter()),SMEP|SMAP|UMIP);
    assert_eq!(common([ALL,ALL & !SMAP].into_iter()),SMEP|UMIP);
    assert_eq!(common([LASS].into_iter()),0);
    assert_eq!(common([ALL,0].into_iter()),0);
    assert_eq!(common([u64::MAX].into_iter()),ALL);
    assert_eq!(shared_entries(ALL,[ALL,0].into_iter()),SMEP|UMIP);
    assert_eq!(shared_entries(ALL,[SMAP].into_iter()),ALL); // parked CPU need not enable other CR4 features
    assert_eq!(shared_entries(SMEP|UMIP,[ALL].into_iter()),SMEP|UMIP);
    println!("x86 access policy: common-CPU intersection, LASS/SMAP dependency and reserved bits passed");
}
