/* Standalone capability probe; never changes an existing directory.
 * Run with MinGW under Wine or native Windows to inspect protected-DACL support.
 * Backend's security assertions and native test selection remain unchanged.
 */
#include <windows.h>
#include <aclapi.h>
#include <stdio.h>
#include <wchar.h>
int main(void) {
 HANDLE token=NULL; DWORD size=0;
 if(!OpenProcessToken(GetCurrentProcess(),TOKEN_QUERY,&token)) return 2;
 GetTokenInformation(token,TokenUser,NULL,0,&size);
 TOKEN_USER *user=HeapAlloc(GetProcessHeap(),0,size);
 if(!GetTokenInformation(token,TokenUser,user,size,&size)) return 3;
 WCHAR path[MAX_PATH]; DWORD length=GetTempPathW(MAX_PATH,path);
 if(!length || length>=MAX_PATH) return 4;
 int written=swprintf(path+length, MAX_PATH-length, L"nudox-dacl-probe-%lu-%llu", GetCurrentProcessId(), (unsigned long long)GetTickCount64());
 if(written<0 || !CreateDirectoryW(path,NULL)) return 4;
 HANDLE file=CreateFileW(path,READ_CONTROL|WRITE_DAC,FILE_SHARE_READ|FILE_SHARE_WRITE|FILE_SHARE_DELETE,NULL,OPEN_EXISTING,FILE_FLAG_BACKUP_SEMANTICS,NULL);
 if(file==INVALID_HANDLE_VALUE) {printf("open_error=%lu\n",GetLastError()); return 4;}
 EXPLICIT_ACCESSW entry={0}; entry.grfAccessPermissions=GENERIC_ALL; entry.grfAccessMode=SET_ACCESS;
 entry.Trustee.TrusteeForm=TRUSTEE_IS_SID; entry.Trustee.TrusteeType=TRUSTEE_IS_USER; entry.Trustee.ptstrName=(LPWSTR)user->User.Sid;
 PACL acl=NULL; DWORD set=SetEntriesInAclW(1,&entry,NULL,&acl);
 if(set) return 5;
 set=SetSecurityInfo(file,SE_FILE_OBJECT,DACL_SECURITY_INFORMATION|PROTECTED_DACL_SECURITY_INFORMATION,NULL,NULL,acl,NULL);
 PSECURITY_DESCRIPTOR sd=NULL; DWORD get=GetSecurityInfo(file,SE_FILE_OBJECT,DACL_SECURITY_INFORMATION,NULL,NULL,NULL,NULL,&sd);
 SECURITY_DESCRIPTOR_CONTROL control=0; DWORD revision=0;
 if(!get) GetSecurityDescriptorControl(sd,&control,&revision);
 printf("set_status=%lu get_status=%lu control=0x%x protected=%d\n",set,get,control,!!(control&SE_DACL_PROTECTED));
 if(sd) LocalFree(sd); LocalFree(acl); CloseHandle(file); RemoveDirectoryW(path);CloseHandle(token);HeapFree(GetProcessHeap(),0,user);
 return set||get?6:0;
}
