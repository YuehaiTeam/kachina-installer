<template>
  <div
    class="input-div"
    :class="[
      props.class,
      {
        active: isFocus,
      },
    ]"
  >
    <input
      class="input-inner"
      v-model="model"
      v-bind="inputProps"
      @focus="isFocus = true"
      @blur="isFocus = false"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue';
import { t } from './i18n';
const isFocus = ref(false);
const model = defineModel<string>();
const props = defineProps({
  placeholder: {
    type: String,
    default: '',
  },
  disabled: {
    type: Boolean,
    default: false,
  },
  type: {
    type: String,
    default: 'text',
  },
  class: {
    type: String,
    default: '',
  },
});
const inputProps = computed(() => {
  const { class: _, placeholder, ...rest } = props;
  return { ...rest, placeholder: placeholder || t('input.placeholder') };
});
</script>

<style scoped></style>
