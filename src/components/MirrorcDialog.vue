<template>
  <Dialog>
    <template #title>
      <div class="title">{{ t('dialog.mirrorcTitle') }}</div>
    </template>
    <template #desc>
      <div class="desc" v-html="t('dialog.mirrorcDesc')"></div>
    </template>
    <template #body>
      <FInput
        class="cdk-input"
        v-model="tempKey"
        type="text"
        :placeholder="t('dialog.mirrorcPlaceholder')"
      />
      <div class="desc">
        <a style="cursor: pointer" @click="openMirrorc">{{
          t('dialog.getCdk')
        }}</a>
      </div>
    </template>
    <template #footer>
      <button
        class="btn btn-install btn-install-2rd neutral"
        @click="$emit('cancel')"
      >
        {{ t('common.cancel') }}
      </button>
      <button class="btn btn-install" :disabled="checking" @click="apply">
        <span
          v-if="checking"
          class="fui-Spinner__spinner"
          style="width: 16px; height: 16px; margin-right: 8px"
        >
          <span class="fui-Spinner__spinnerTail"></span>
        </span>
        {{ t('common.confirm') }}
      </button>
    </template>
  </Dialog>
</template>

<script lang="ts" setup>
import { ref, watch } from 'vue';
import Dialog from '../Dialog.vue';
import FInput from '../FInput.vue';
import { invoke } from '../tauri';
import { type MirrorcUpdate } from '../api/ipc';
import { processMirrorcError } from '../mirrorc-errors';
import { dialogError } from '../ui';
import { t } from '../i18n';

const props = defineProps<{
  appName: string;
  sourceUrl: string;
  initialKey: string;
}>();

const emit = defineEmits<{
  cancel: [];
  applied: [payload: { url: string; key: string }];
}>();

const tempKey = ref(props.initialKey);
const checking = ref(false);

watch(
  () => props.initialKey,
  (key) => {
    tempKey.value = key;
  },
);

function credTarget() {
  return `KachinaInstaller_MirrorChyanCDK_${props.appName}`;
}

function openMirrorc() {
  invoke('launch', {
    path: `https://mirrorchyan.com/?source=Kachina${props.appName}`,
  });
}

async function apply() {
  if (!tempKey.value) {
    try {
      await invoke('wincred_delete', { target: credTarget() });
    } catch (e) {
      console.warn(e);
    }
    emit('applied', { url: props.sourceUrl, key: '' });
    return;
  }
  if (checking.value) return;
  checking.value = true;
  const sourceUrl = new URL(props.sourceUrl);
  if (!sourceUrl.hostname) {
    await dialogError(
      t('err.mirrorcBadSource', { url: props.sourceUrl }),
      t('common.error'),
    );
    checking.value = false;
    return;
  }
  const status = await invoke<MirrorcUpdate>('get_mirrorc_status', {
    resourceId: sourceUrl.hostname,
    cdk: tempKey.value,
    currentVersion: '',
    channel: sourceUrl.searchParams.get('channel') || 'stable',
    arch: sourceUrl.searchParams.get('arch') || undefined,
    os: sourceUrl.searchParams.get('os') || undefined,
  });
  const errorResult = processMirrorcError(status, 'cdk-validation');
  if (errorResult) {
    await dialogError(errorResult.message, t('common.error'));
    checking.value = false;
    return;
  }
  try {
    await invoke('wincred_write', {
      target: credTarget(),
      token: tempKey.value,
      comment: 'MirrorChyan CDK for BetterGI',
    });
  } catch (e) {
    console.warn(e);
  }
  emit('applied', { url: props.sourceUrl, key: tempKey.value });
  checking.value = false;
}
</script>

<style scoped>
.title {
  font-size: 25px;
  padding: 2px 10px 6px;
}
.desc {
  font-size: 14px;
  opacity: 0.8;
  padding-left: 10px;
  padding-bottom: 2px;
}
.cdk-input {
  app-region: no-drag;
  margin: 30px 10px;
  margin-bottom: 48px;
  width: 320px;
}
.cdk-input :deep(input) {
  font-family: Consolas, monospace !important;
}
.btn-install {
  app-region: no-drag;
  height: 40px;
  width: 140px;
  position: absolute;
  bottom: 20px;
  right: 8px;
  &.btn-install-2rd {
    right: 158px;
    width: 100px;
  }
}
</style>
