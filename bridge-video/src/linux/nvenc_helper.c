/*
 * NVIDIA Jetson hardware encoder via GStreamer pipeline
 *
 * Uses gst-launch-1.0 as a subprocess with nvv4l2h264enc/nvv4l2h265enc.
 * Raw NV12 frames are piped to stdin, encoded H.264/H.265 NAL units
 * come from stdout. GStreamer handles all V4L2 buffer management internally.
 *
 * Pipeline: fdsrc ! rawvideoparse ! nvvidconv ! nvv4l2h264enc ! h264parse ! fdsink
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <sys/wait.h>

struct nvenc_state {
    pid_t pid;
    int write_fd;   /* pipe to gst stdin (NV12 frames in) */
    int read_fd;    /* pipe from gst stdout (encoded out) */
    int width;
    int height;
    unsigned long long frame_num;
};

void *nvenc_create(int width, int height, int codec, int bitrate, int fps) {
    struct nvenc_state *enc = calloc(1, sizeof(*enc));
    if (!enc) return NULL;

    enc->width = width;
    enc->height = height;

    int pipe_in[2];   /* parent writes NV12 to pipe_in[1], child reads from pipe_in[0] */
    int pipe_out[2];  /* child writes encoded to pipe_out[1], parent reads from pipe_out[0] */

    if (pipe(pipe_in) < 0 || pipe(pipe_out) < 0) {
        fprintf(stderr, "nvenc: pipe failed: %s\n", strerror(errno));
        free(enc);
        return NULL;
    }

    /* Build GStreamer pipeline string */
    const char *enc_element = codec ? "nvv4l2h265enc" : "nvv4l2h264enc";
    const char *parse_element = codec ? "h265parse" : "h264parse";

    char pipeline[1024];
    snprintf(pipeline, sizeof(pipeline),
        "gst-launch-1.0 -q "
        "fdsrc fd=%d ! "
        "rawvideoparse width=%d height=%d format=nv12 framerate=%d/1 ! "
        "nvvidconv ! "
        "%s bitrate=%d maxperf-enable=true control-rate=1 "
        "iframeinterval=%d insert-sps-pps=true ! "
        "%s config-interval=-1 ! "
        "fdsink fd=%d",
        pipe_in[0], width, height, fps,
        enc_element, bitrate, fps * 2,
        parse_element,
        pipe_out[1]);

    fprintf(stderr, "nvenc: launching: %s\n", pipeline);

    pid_t pid = fork();
    if (pid < 0) {
        fprintf(stderr, "nvenc: fork failed: %s\n", strerror(errno));
        free(enc);
        return NULL;
    }

    if (pid == 0) {
        /* Child: redirect and exec gstreamer */
        close(pipe_in[1]);   /* close write end */
        close(pipe_out[0]);  /* close read end */

        /* Close other FDs we don't need */
        /* pipe_in[0] and pipe_out[1] are passed as fd args in the pipeline string */

        execl("/bin/sh", "sh", "-c", pipeline, NULL);
        _exit(127);
    }

    /* Parent */
    close(pipe_in[0]);   /* close read end */
    close(pipe_out[1]);  /* close write end */

    /* Set output pipe to non-blocking for reading */
    int flags = fcntl(pipe_out[0], F_GETFL, 0);
    fcntl(pipe_out[0], F_SETFL, flags | O_NONBLOCK);

    enc->pid = pid;
    enc->write_fd = pipe_in[1];
    enc->read_fd = pipe_out[0];

    /* Wait for GStreamer to initialize — NVIDIA encoder takes ~1-2s */
    usleep(2000000);

    /* Check if child is still alive */
    int status;
    pid_t result = waitpid(pid, &status, WNOHANG);
    if (result != 0) {
        fprintf(stderr, "nvenc: GStreamer pipeline failed to start\n");
        close(pipe_in[1]);
        close(pipe_out[0]);
        free(enc);
        return NULL;
    }

    fprintf(stderr, "nvenc: encoder ready (pid=%d)\n", pid);
    return enc;
}

int nvenc_encode_frame(void *handle, const unsigned char *nv12_data, int nv12_size,
                       unsigned char *out_data, int out_max_size, int force_keyframe) {
    struct nvenc_state *enc = (struct nvenc_state *)handle;
    if (!enc) return -1;

    /* Write NV12 frame to GStreamer stdin */
    int written = 0;
    while (written < nv12_size) {
        int n = write(enc->write_fd, nv12_data + written, nv12_size - written);
        if (n < 0) {
            if (errno == EAGAIN || errno == EINTR) continue;
            fprintf(stderr, "nvenc: write failed: %s\n", strerror(errno));
            return -1;
        }
        written += n;
    }

    enc->frame_num++;

    /* Read any available encoded output (non-blocking) */
    struct pollfd pfd;
    pfd.fd = enc->read_fd;
    pfd.events = POLLIN;
    int poll_ret = poll(&pfd, 1, 100);  /* 100ms timeout — encoder pipeline has latency */

    if (poll_ret > 0 && (pfd.revents & POLLIN)) {
        int total_read = 0;
        while (total_read < out_max_size) {
            int n = read(enc->read_fd, out_data + total_read, out_max_size - total_read);
            if (n <= 0) break;
            total_read += n;

            /* Check if more data available immediately */
            int more = poll(&pfd, 1, 0);
            if (more <= 0) break;
        }
        return total_read;
    }

    return 0;
}

void nvenc_destroy(void *handle) {
    struct nvenc_state *enc = (struct nvenc_state *)handle;
    if (!enc) return;

    close(enc->write_fd);
    close(enc->read_fd);

    if (enc->pid > 0) {
        kill(enc->pid, SIGTERM);
        int status;
        waitpid(enc->pid, &status, 0);
    }

    free(enc);
    fprintf(stderr, "nvenc: destroyed\n");
}
