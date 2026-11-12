#include <pthread.h>
#include <malloc.h>
#include <string.h>
#include <unistd.h>
#include <stdlib.h>
#include <stdio.h>
#define PX 10406880u
#define FILT 10409289u
static char* q[4]; static int qn; static pthread_mutex_t qm=PTHREAD_MUTEX_INITIALIZER; static pthread_cond_t qc=PTHREAD_COND_INITIALIZER;
static void* enc(void* x){ (void)x;
  char*f=malloc(FILT); if(!f) return 0; memset(f,0x5a,FILT);
  for(int i=0;i<5;i++){ char*c=malloc(1<<20); memset(c,0x5a,1<<20);
    pthread_mutex_lock(&qm); while(qn==4) pthread_cond_wait(&qc,&qm); q[qn++]=c; pthread_cond_signal(&qc); pthread_mutex_unlock(&qm); }
  free(f); return 0; }
static void* drain(void* x){ (void)x; int idle=0;
  while(idle<3){ pthread_mutex_lock(&qm); if(qn){ char*c=q[--qn]; pthread_cond_signal(&qc); pthread_mutex_unlock(&qm); free(c); idle=0;} else { pthread_mutex_unlock(&qm); idle++; struct timespec ts={0,1000000}; nanosleep(&ts,0);} }
  return 0; }
int main(int argc,char**argv){
  const char*m=argc>1?argv[1]:"none";
  if(!strcmp(m,"mmap")||!strcmp(m,"mmap_trim")){ printf("mallopt mmap=%d\n", mallopt(M_MMAP_THRESHOLD,128*1024)); mallopt(M_TRIM_THRESHOLD,128*1024); }
  if(!strcmp(m,"arena")||!strcmp(m,"arena_trim")){ printf("mallopt arena=%d\n", mallopt(M_ARENA_MAX,1)); }
  if(!strcmp(m,"trim_thr")) mallopt(M_TRIM_THRESHOLD,128*1024);
  for(int cap=0;cap<2;cap++){
    char*pix=malloc(PX); memset(pix,0x5a,PX);
    char*canvas=malloc(PX); memset(canvas,0x5a,PX);
    pthread_t t,d; pthread_create(&t,0,enc,0); pthread_create(&d,0,drain,0);
    pthread_join(t,0); pthread_join(d,0);
    free(pix); free(canvas);
    if(!strcmp(m,"trim")||!strcmp(m,"mmap_trim")||!strcmp(m,"arena_trim")) malloc_trim(0);
  }
  sleep(20); return 0; }
