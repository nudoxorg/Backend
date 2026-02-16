## Organizations
**nudox** - name of organization. this hosts the data products (all databases)
**devtest** - organization for testing/active development

## Users
prisma -> read-only - connected to frontend/middle services.. Does not need write perms
tcotrPrismaCe^4
onyx -> write from compiler output. - only used on server
B0tbN1ght^

## Roles

'all_non_destructive' has id: 'Role/all_non_destructive'
  and actions: `\[branch,clone,commit_read_access,commit_write_access,create_database,fetch,instance_read_access,instance_write_access,meta_read_access,meta_write_access,push,rebase,schema_read_access,schema_write_access\]`
- linked to onyx

'read_all' has id: 'Role/read_all'
  and actions: `[commit_read_access,instance_read_access,meta_read_access,schema_read_access]`
- linked with prisma


## TDB Setup
create db `terminusdb organization create nudox` also create devtest org
read-user `terminusdb user create prisma`
write-user `terminusdb user create onyx`
write-role `terminusdb role create all_non_destructive create_database clone fetch push branch rebase instance_read_access instance_write_access schema_read_access schema_write_access meta_read_access meta_write_access commit_read_access commit_write_access`
read-role `terminusdb role create read_all instance_read_access schema_read_access meta_read_access commit_read_access`
grant capability write (user, role, db) `terminusdb capability grant onyx nudox all_non_destructive --scope-type=organization` for nudox, add devtest as well
same for read `terminusdb capability grant prisma nudox read_all --scope-type=organization` run again for devtest
password for user `terminusdb user password prisma -p tcotrPrismaCe^4` - doesnt have to be super secure
password for user `terminusdb user password onyx -p B0tbN1ght^`


