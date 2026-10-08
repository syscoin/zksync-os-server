"""SYSCOIN: Compile REAL candidate parser/predicate verbatim with synthetic surrounding types only."""
import hashlib, pathlib, re, shutil, subprocess, tempfile, unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
PATCH = ROOT / 'scripts/patches/zksync-era-syscoin.patch'
FIXTURE = ROOT / 'scripts/fixtures/zkstack-forge.upstream.rs'

def checked_sources():
    original = FIXTURE.read_bytes()
    if hashlib.sha256(original).hexdigest() != '7236c0f0839088de0a740f3306eecc43b4cb51579e6ec414001775709ac48e8d':
        raise AssertionError('Exact upstream Forge fixture differs')
    patch = PATCH.read_text()
    target = 'zkstack_cli/crates/common/src/forge.rs'
    section = patch.split(f'diff --git a/{target} b/{target}\n', 1)[1].split('\ndiff --git ', 1)[0]
    lines = original.decode().splitlines(keepends=True)
    result, cursor = [], 0
    for header, body in re.findall(r'(@@ -\d+(?:,\d+)? \+\d+(?:,\d+)? @@[^\n]*)\n(.*?)(?=\n@@ |\Z)', section, re.S):
        start, count = re.match(r'@@ -(\d+)(?:,(\d+))?', header).groups()
        count = int(count) if count is not None else 1
        position = int(start) - (1 if count else 0)
        removed = [line[1:] + '\n' for line in body.splitlines() if line.startswith('-')]
        added = [line[1:] + '\n' for line in body.splitlines() if line.startswith('+')]
        if cursor > position or len(removed) != count or lines[position:position + count] != removed:
            raise AssertionError('Exact upstream Forge hunk differs')
        result.extend(lines[cursor:position]); result.extend(added); cursor = position + count
    result.extend(lines[cursor:])
    forge = ''.join(result)
    if hashlib.sha256(forge.encode()).hexdigest() != '0ad4888aae93b44087d183944146b2dccd69a9ee4903c1b5764c080db19a8b37':
        raise AssertionError('Complete Forge postimage differs')
    target = 'zkstack_cli/crates/zkstack/src/commands/chain/utils.rs'
    section = patch.split(f'diff --git a/{target} b/{target}\n', 1)[1].split('\ndiff --git ', 1)[0]
    added = '\n'.join(line[1:] for line in section.splitlines()
                      if line.startswith('+') and not line.startswith('+++'))
    predicate = function(added, 'fn validate_migration_receipt(')
    if hashlib.sha256(predicate.encode()).hexdigest() != 'fe444b1c8f7f63817ddacaff48b6d9325191cde893fb7a644244b87933821592':
        raise AssertionError('Verbatim receipt predicate differs')
    return forge, added

def function(source, marker):
    start = source.index(marker)
    at = source.index('{', start)
    depth = 0
    for end in range(at, len(source)):
        if source[end] == '{': depth += 1
        elif source[end] == '}':
            depth -= 1
            if depth == 0: return source[start:end + 1]
    raise AssertionError('Unclosed real source function')

def constant(source, name):
    start = source.index('const ' + name + ':')
    return source[start:source.index('];', start) + 2]


support = r'''
use std::{path::Path,str::FromStr};
#[macro_export] macro_rules! bail { ($($arg:tt)*) => { return Err(format!($($arg)*)) }; }
#[macro_export] macro_rules! make_error { ($($arg:tt)*) => { format!($($arg)*) }; }
mod anyhow {pub use crate::{bail,make_error as anyhow};pub type Result<T>=std::result::Result<T,String>;}
trait Context<T>{fn context(self,message:&str)->anyhow::Result<T>;}
impl<T> Context<T> for Option<T>{fn context(self,message:&str)->anyhow::Result<T>{self.ok_or_else(||message.into())}}
#[derive(Clone,Copy,Debug,PartialEq,Eq)] struct Address([u8;20]);
impl FromStr for Address {type Err=();fn from_str(s:&str)->Result<Self,Self::Err>{
 let s=s.strip_prefix("0x").ok_or(())?;if s.len()!=40{return Err(())}let mut a=[0u8;20];
 for i in 0..20 {a[i]=u8::from_str_radix(&s[i*2..i*2+2],16).map_err(|_|())?;}Ok(Self(a))}}
#[derive(Clone,Copy,Debug,PartialEq,Eq)] struct H256([u8;32]);
#[derive(Clone,Copy,Debug,PartialEq,Eq)] struct U256(u64);
impl From<u64> for U256{fn from(n:u64)->Self{Self(n)}}
mod ethers {pub mod types {#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub struct U64(pub u64);
 impl From<u64> for U64{fn from(n:u64)->Self{Self(n)}}impl U64{pub fn as_u64(self)->u64{self.0}}}}
use ethers::types::U64;
#[derive(Clone,Debug)] struct Bytes(Vec<u8>);impl AsRef<[u8]> for Bytes{fn as_ref(&self)->&[u8]{&self.0}}
#[derive(Clone,Debug)]struct TransactionReceipt{status:Option<U64>,transaction_hash:H256,from:Address,to:Option<Address>,
 transaction_index:U64,block_number:Option<U64>,block_hash:Option<H256>}
#[derive(Clone,Debug)]struct Transaction{hash:H256,from:Address,to:Option<Address>,nonce:U256,value:U256,input:Bytes,
 chain_id:Option<U256>,block_number:Option<U64>,block_hash:Option<H256>,transaction_index:Option<U64>}
#[derive(Clone,Debug)]struct Block<T>{number:Option<U64>,hash:Option<H256>,transactions:Vec<T>}
#[derive(Default)]struct ForgeScriptArgs{additional_args:Vec<String>}
'''
parser_tests = r'''
fn args(items:&[&str])->ForgeScriptArgs{ForgeScriptArgs{additional_args:items.iter().map(|s|s.to_string()).collect()}}
const SENDER:&str="0x622a54ea3a123127ca5fe8b98de90e957471093a";
#[test]fn protected_named_account_real_body(){
 let split=args(&["--account","zksys-admin","--password-file","/private/operator.password","--sender",SENDER]);
 let equal=args(&["--sender=0x622a54ea3a123127ca5fe8b98de90e957471093a","--account=zksys-admin","--password-file=/private/operator.password"]);
 let expected=(vec!["--account".into(),"zksys-admin".into(),"--password-file".into(),"/private/operator.password".into()],SENDER.parse::<Address>().unwrap());
 assert_eq!(split.protected_account_args().unwrap(),Some(expected.clone()));assert_eq!(equal.protected_account_args().unwrap(),Some(expected));
 assert_eq!(args(&[]).protected_account_args().unwrap(),None);
 assert_eq!(args(&["--ffi"]).protected_account_args().unwrap(),None);
 for bad in [vec!["--account"],vec!["--account","zksys-admin"],vec!["--account","zksys-admin","--password-file","relative","--sender",SENDER],
  vec!["--account","../admin","--password-file","/private/pw","--sender",SENDER],
  vec!["--account","zksys-admin","--password-file","/private/pw","--sender","bad"],
  vec!["--account","zksys-admin","--password-file","/private/pw","--sender",SENDER,"--nonce=2"],
  vec!["--account","zksys-admin","--account=other","--password-file","/private/pw","--sender",SENDER],
  vec!["--accounts=zksys-admin"],vec!["--keystore=/private/key"],vec!["--ledger"],
  vec!["--account=","--password-file=/private/pw","--sender=0x622a54ea3a123127ca5fe8b98de90e957471093a"],
  vec!["--account",".","--password-file","/private/pw","--sender",SENDER],
  vec!["--account","..","--password-file","/private/pw","--sender",SENDER],
  vec!["--account","bad\\name","--password-file","/private/pw","--sender",SENDER]] {
  assert!(args(&bad).protected_account_args().is_err(),"bad protected selector accepted");
 }
 for flag in ["--private-key","--private-keys","--mnemonics","--mnemonic-passphrases","--mnenomic-passphrases","--password"] {
  for value in [format!("{flag}=PRIVATE_SECRET_MARKER"),format!("{flag} PRIVATE_SECRET_MARKER")] {
   let error=args(&[&value]).protected_account_args().unwrap_err();assert!(!error.contains("PRIVATE_SECRET_MARKER"));assert!(error.contains(flag));
  }
  let error=args(&[flag,"PRIVATE_SECRET_MARKER"]).protected_account_args().unwrap_err();assert!(!error.contains("PRIVATE_SECRET_MARKER"));
 }
 assert!(!argument_uses_flag("--password-file=/private/pw","--password"));
}
'''
receipt_tests = r'''
#[derive(Clone)]struct Case{receipt:TransactionReceipt,tx:Transaction,block:Block<H256>,to:Address,data:Vec<u8>,value:U256,sender:Address,nonce:U256,chain:u64,hash:H256}
fn fixture()->Case{let hash=H256([1;32]);let bh=H256([2;32]);let sender=Address([3;20]);let to=Address([4;20]);let number=Some(U64(985703));
 Case{receipt:TransactionReceipt{status:Some(U64(1)),transaction_hash:hash,from:sender,to:Some(to),transaction_index:U64(1),block_number:number,block_hash:Some(bh)},
 tx:Transaction{hash,from:sender,to:Some(to),nonce:U256(171),value:U256(75),input:Bytes(vec![1,2,3]),chain_id:Some(U256(5700)),block_number:number,block_hash:Some(bh),transaction_index:Some(U64(1))},
 block:Block{number,hash:Some(bh),transactions:vec![H256([9;32]),hash]},to,data:vec![1,2,3],value:U256(75),sender,nonce:U256(171),chain:5700,hash}}
fn audit(c:&Case)->anyhow::Result<()>{validate_migration_receipt(&c.receipt,&c.tx,&c.block,c.to,&c.data,c.value,c.sender,c.nonce,c.chain,c.hash)}
#[test]fn exact_receipt_predicate_real_body(){
 let valid=fixture();assert!(audit(&valid).is_ok());
 let mutations:Vec<Box<dyn Fn(&mut Case)>>=vec![
  Box::new(|c|c.receipt.status=None),Box::new(|c|c.receipt.status=Some(U64(0))),Box::new(|c|c.receipt.transaction_hash=H256([8;32])),
  Box::new(|c|c.receipt.from=Address([8;20])),Box::new(|c|c.receipt.to=None),Box::new(|c|c.receipt.to=Some(Address([8;20]))),
  Box::new(|c|c.tx.hash=H256([8;32])),Box::new(|c|c.tx.from=Address([8;20])),Box::new(|c|c.tx.to=None),
  Box::new(|c|c.tx.nonce=U256(172)),Box::new(|c|c.tx.value=U256(76)),Box::new(|c|c.tx.input=Bytes(vec![1,2,4])),
  Box::new(|c|c.tx.chain_id=None),Box::new(|c|c.tx.chain_id=Some(U256(1))),Box::new(|c|c.tx.block_hash=None),
  Box::new(|c|c.tx.block_number=None),Box::new(|c|c.tx.transaction_index=Some(U64(0))),
  Box::new(|c|c.receipt.block_number=None),Box::new(|c|c.block.number=Some(U64(985704))),Box::new(|c|c.block.hash=None),
  Box::new(|c|c.block.hash=Some(H256([8;32]))),Box::new(|c|c.block.transactions.clear()),
  Box::new(|c|c.block.transactions[1]=H256([8;32])),Box::new(|c|c.block.transactions.push(c.hash))];
 for mutate in mutations{let mut c=valid.clone();mutate(&mut c);assert!(audit(&c).is_err(),"bad canonical receipt accepted");}
}
'''
class MigrationAccountArgumentTests(unittest.TestCase):
    def test_exact_checked_sources_are_reconstructed(self):
        forge, utils = checked_sources()
        self.assertIn('pub fn protected_account_args(', forge)
        self.assertIn('fn validate_migration_receipt(', utils)

    def test_verbatim_real_functions_with_explicit_fixture_surroundings(self):
        rustc = shutil.which('rustc')
        if not rustc:
            self.skipTest('rustc unavailable; exact checked source pins are verified separately')
        available = subprocess.run([rustc, '--version'], capture_output=True, timeout=15)
        if available.returncode:
            self.skipTest('Configured rustc unavailable; no full CLI build is claimed')
        forge, utils = checked_sources()
        parts = [function(forge, 'fn argument_uses_flag('),
                 function(forge, 'fn reject_raw_secret_args('),
                 function(forge[forge.index('impl ForgeScriptArgs {'):], 'pub fn wallet_args_passed('),
                 function(forge, 'pub fn protected_account_args('),
                 function(utils, 'fn validate_migration_receipt(')]
        rust = (support + constant(forge, 'WALLET_ARGS') + '\n'
                + constant(forge, 'RAW_SECRET_ARGS') + '\n' + parts[0]
                + '\nimpl ForgeScriptArgs {\n' + '\n'.join(parts[1:4])
                + '\n}\n' + parts[4] + parser_tests + receipt_tests)
        with tempfile.TemporaryDirectory(prefix='migration-real-functions-tests-') as directory:
            binary = str(pathlib.Path(directory) / 'real-source-tests')
            built = subprocess.run([rustc, '--edition=2021', '--test', '-', '-o', binary],
                                   input=rust, text=True, capture_output=True, timeout=60)
            self.assertEqual(built.returncode, 0, built.stderr)
            result = subprocess.run([binary], capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn('2 passed', result.stdout)


if __name__ == '__main__':
    unittest.main()
